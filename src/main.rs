#![warn(
	clippy::all,
	clippy::nursery,
	clippy::cargo,
	clippy::redundant_closure_for_method_calls
)]

use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use cursive::traits::*;
use cursive::view::ScrollStrategy;
use cursive::views::{DebugView, Dialog, EditView, LinearLayout, Panel, SelectView, TextView};
use cursive::Cursive;

use ini::Ini;

use log::{error, info, warn};

use notify::{DebouncedEvent, RecursiveMode, Watcher};

const BACKUP_FOLDER: &str = "save-manager";

const EXTENSION: &str = ".ck2";

const OPTIONS: [&str; 7] = [
	"Set a new working game",
	"Make a new backup",
	"Make a new backup (with note)",
	"Restore a backup",
	"Automatically take backups",
	"Delete old backups",
	"Quit",
];

fn main() {
	let mut root = cursive::default();
	cursive::logger::init();

	// get the location of the config file
	let config_path = env::current_exe()
		.unwrap()
		.parent()
		.unwrap()
		.join("conf.ini");

	// get config file, and create one if it does not exist
	root.set_user_data(match Ini::load_from_file(&config_path) {
		Ok(config) => config,
		Err(_) => Ini::new(),
	});

	//
	// set up paths
	//

	// the user may optionally specify a path directly
	let args: Vec<String> = env::args().collect();

	// get the location of the "save games" directory
	let save_path = if args.len() > 1 {
		Path::new(args[1].as_str()).to_path_buf()
	} else {
		env::current_exe()
			.unwrap()
			.parent()
			.unwrap()
			.parent()
			.unwrap()
			.parent()
			.unwrap()
			.join("save games")
	};

	if !save_path.is_dir() {
		root.add_layer(
			Dialog::around(
				TextView::new("Executable must either be located in the ../Crusader Kings II/mod/save-manager/ directory, or have a directory path as an argument.")
			)
			.button("Ok", Cursive::quit)
		);
	} else {
		// create the backup directory if it does not exist
		let backup_path = save_path.join(BACKUP_FOLDER);
		if !backup_path.is_dir() {
			if let Err(e) = fs::create_dir(&backup_path) {
				root.add_layer(
					Dialog::around(TextView::new(format!("Error occurred: {}", e.to_string())))
						.button("Ok", Cursive::quit),
				);
			}
		}

		if backup_path.is_dir() {
			//
			// set up UI
			//

			// set up the logging panel
			let log_view = DebugView::new()
				.scrollable()
				.scroll_strategy(ScrollStrategy::StickToBottom);

			// set up the main screen for user interaction
			let mut main_view = SelectView::<String>::new()
				.on_submit(move |s, option| select_option(s, option, &save_path, &backup_path))
				.autojump();
			main_view.add_all_str(OPTIONS.to_vec());

			root.add_fullscreen_layer(
				LinearLayout::horizontal()
					.child(Panel::new(main_view).full_screen())
					.child(Panel::new(log_view).full_screen())
					.full_screen(),
			);
		}
	}

	info!("Started CK2 Save Manager");

	root.run();
}

fn select_option(s: &mut Cursive, option: &str, save_path: &Path, backup_path: &Path) {
	if let Err(e) = match option {
		"Set a new working game" => set_game(s, save_path),
		"Make a new backup" => backup(s, save_path, backup_path, false),
		"Make a new backup (with note)" => backup(s, save_path, backup_path, true),
		"Restore a backup" => restore(s, save_path, backup_path),
		"Automatically take backups" => auto(s, save_path, backup_path),
		// "Delete old backups" => delete(s, backup_path),
		"Quit" => {
			s.quit();
			Ok(())
		}
		_ => unimplemented!(),
	} {
		s.add_layer(
			Dialog::around(TextView::new(format!("Error occurred: {}", e))).button("Ok", |s| {
				s.pop_layer();
			}),
		);
	}
}

fn set_game(s: &mut Cursive, save_path: &Path) -> Result<(), Box<dyn Error>> {
	let save_files = fs::read_dir(save_path)?
		.filter_map(Result::ok)
		.filter(|file| file.path().is_file())
		.filter_map(|file| {
			file.path()
				.file_stem()
				.and_then(OsStr::to_str)
				.map(|stem| stem.to_string())
		});

	let file_selection_dialog = Dialog::around(
		SelectView::<String>::new()
			.with_all_str(save_files)
			.on_submit(|s: &mut Cursive, save_file: &String| {
				s.with_user_data(|config: &mut Ini| {
					config.with_general_section().set("save_file", save_file);
					config
						.write_to_file(
							env::current_exe()
								.unwrap()
								.parent()
								.unwrap()
								.join("conf.ini"),
						)
						.unwrap();
				});

				info!("Save file set to: {}", save_file);

				s.pop_layer();
			}),
	)
	.title("Select game save")
	.button("Manually enter save name", |s| {
		s.pop_layer();

		let manual_entry = Dialog::around(EditView::new().on_submit(|s, save_file| {
			if save_file.is_empty() {
				s.add_layer(
					Dialog::around(TextView::new("Enter a name of a save game.")).button(
						"Ok",
						|s| {
							s.pop_layer();
						},
					),
				)
			} else {
				s.with_user_data(|config: &mut Ini| {
					config.with_general_section().set("save_file", save_file);
					config
						.write_to_file(
							env::current_exe()
								.unwrap()
								.parent()
								.unwrap()
								.join("conf.ini"),
						)
						.unwrap();
				});

				warn!("Save file manually set to: {}", save_file);

				s.pop_layer();
			}
		}))
		.button("Cancel", |s| {
			s.pop_layer();
		});

		s.add_layer(manual_entry)
	});

	s.add_layer(file_selection_dialog);
	Ok(())
}

fn backup(
	s: &mut Cursive,
	save_path: &Path,
	backup_path: &Path,
	has_note: bool,
) -> Result<(), Box<dyn Error>> {
	let config: &mut Ini = s
		.user_data()
		.expect("User data not set up correctly on program start");
	let mut general = config.with_general_section();
	let file_to_backup = general
		.get("save_file")
		.ok_or("No file has been set to backup.")?;

	let file_path = save_path.join(file_to_backup.to_string() + EXTENSION);

	if !file_path.is_file() {
		s.add_layer(
			Dialog::around(TextView::new("Save file not found.")).button("Ok", |s| {
				s.pop_layer();
			}),
		)
	} else {
		let backup_dir = backup_path.join(file_to_backup);
		if !backup_dir.is_dir() {
			fs::create_dir(&backup_dir)?;
		}

		if has_note {
			let file_path_copy = file_path.clone();
			let backup_dir_copy = backup_dir.clone();

			s.add_layer(
				Dialog::around(
					EditView::new()
						.on_submit(move |s, note| {
							if let Err(e) = backup_core(&file_path, &backup_dir, note) {
								error!("{}", e);
							}
							s.pop_layer();
						})
						.with_name("note"),
				)
				.button("Cancel", |s| {
					s.pop_layer();
				})
				.button("Enter", move |s| {
					let note = s
						.call_on_name("note", |view: &mut EditView| view.get_content())
						.expect("EditView not created for user note entry");
					if let Err(e) = backup_core(&file_path_copy, &backup_dir_copy, &note) {
						error!("{}", e);
					}
					s.pop_layer();
				}),
			);
		} else {
			backup_core(&file_path, &backup_dir, "")?;
		}
	}

	Ok(())
}

fn backup_core(file_path: &Path, backup_dir: &Path, note: &str) -> Result<(), Box<dyn Error>> {
	let save_number = fs::read_dir(&backup_dir)?
		.filter_map(Result::ok)
		.filter(|file| file.path().is_file())
		.filter_map(|file| file.file_name().to_str().map(|file| file.to_string()))
		.filter_map(|file| file.splitn(2, '_').next().unwrap().parse::<usize>().ok())
		.max();

	let save_number = match save_number {
		Some(x) => x + 1,
		None => 1,
	};

	if note.is_empty() {
		fs::copy(file_path, backup_dir.join(save_number.to_string()))
	} else {
		fs::copy(
			file_path,
			backup_dir.join(save_number.to_string() + "_" + note.trim()),
		)
	}?;

	info!("Backup number {} created", save_number);

	Ok(())
}

fn restore(s: &mut Cursive, save_path: &Path, backup_path: &Path) -> Result<(), Box<dyn Error>> {
	let config: &mut Ini = s.user_data().expect("User data not set up correctly on program start");
	let mut general = config.with_general_section();
	let file_to_backup = general
		.get("save_file")
		.ok_or("No save file has been set.")?;

	let save_destination = save_path.join(file_to_backup.to_string() + EXTENSION);
	let game_backup_folder = backup_path.join(file_to_backup);

	let backup_selection = SelectView::<String>::new()
		.with_all_str({
			let mut items = fs::read_dir(backup_path.join(file_to_backup))?
				.filter_map(Result::ok)
				.filter(|file| file.path().is_file())
				.filter_map(|file| file.file_name().to_str().map(|file| file.to_string()))
				.filter(|file| file.splitn(2, '_').next().unwrap().parse::<usize>().is_ok())
				.collect::<Vec<String>>();
			items.sort_unstable_by_key(|key| {
				key.splitn(2, '_').next().unwrap().parse::<usize>().unwrap()
			});
			items
		})
		.on_submit(move |s: &mut Cursive, backup: &String| {
			match fs::copy(game_backup_folder.join(backup), &save_destination) {
				Ok(_) => {
					s.pop_layer();
				}
				Err(e) => s.add_layer(Dialog::around(TextView::new(format!(
					"Error occurred: {}",
					e
				)))),
			}
		})
		.autojump()
		.scrollable();

	s.add_layer(Dialog::around(backup_selection).button("Cancel", |s| {
		s.pop_layer();
	}));

	Ok(())
}

fn auto(s: &mut Cursive, save_path: &Path, backup_path: &Path) -> Result<(), Box<dyn Error>> {
	let config: &mut Ini = s.user_data().expect("User data not set up correctly on program start");
	let mut general = config.with_general_section();
	let file_to_backup = general
		.get("save_file")
		.ok_or("No save file has been set.")?;

	let (tx, rx) = mpsc::channel();
	let mut watcher = notify::watcher(tx, Duration::from_secs(10))?;
	watcher.watch(
		save_path.join(file_to_backup.to_string() + EXTENSION),
		RecursiveMode::NonRecursive,
	)?;

	let file_path = save_path.join(file_to_backup.to_string() + EXTENSION);

	if !file_path.is_file() {
		s.add_layer(
			Dialog::around(TextView::new("Save file not found.")).button("Ok", |s| {
				s.pop_layer();
			}),
		)
	} else {
		let backup_dir = backup_path.join(file_to_backup);
		if !backup_dir.is_dir() {
			fs::create_dir(&backup_dir)?;
		}

		thread::spawn(move || loop {
			match rx.recv() {
				Ok(event) => {
					if let DebouncedEvent::Write(_) = event {
						if let Err(e) = backup_core(&file_path, &backup_dir, "") {
							error!("{}", e);
							break;
						}
					}
				}
				Err(e) => {
					warn!("{}", e);
					break;
				}
			}
		});

		// this is needed to see new backup log messages without user input
		s.set_fps(1);

		let cancel_dialog = Dialog::around(TextView::new("Automatically backing up save files..."))
			.button("Cancel", move |s| {
				// prevent the watcher from being dropped until the dialog is dismissed
				&watcher;

				info!("Stopped automatic backups");
				s.set_fps(0);
				s.pop_layer();
			});

		s.add_layer(cancel_dialog);
	}

	Ok(())
}

// fn delete(s: &mut Cursive, backup_path: &Path) -> Result<(), Box<dyn Error>> {}
