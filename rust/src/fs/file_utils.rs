use std::fs::File;
use std::io;
use std::io::{Write};
use std::path::{PathBuf};
use std::str;
use automerge::{Automerge, ChangeHash, ObjType, ReadDoc};
use automerge::ObjId;
use md5::Digest;
use samod::{DocumentId};
use crate::helpers::doc_utils::SimpleDocReader;
use crate::helpers::utils::{parse_automerge_url};

use crate::parser::godot_parser::{GodotScene, parse_scene, recognize_scene};

#[derive(Debug, Clone, PartialEq)]
pub enum FileContent {
	String(String),
	Binary(Vec<u8>),
	Scene(GodotScene),
	Deleted,
}

#[derive(Debug)]
pub enum FileSystemEvent {
    FileCreated(PathBuf, FileContent),
    FileModified(PathBuf, FileContent),
    FileDeleted(PathBuf),
}

impl FileContent {
	// Write file content to disk
	async fn write_file_content(path: &PathBuf, content: &FileContent) -> std::io::Result<Digest> {
		let temp_text;
		// Write the content based on its type
		let buf: &[u8] = match content {
			FileContent::String(text) => {
				text.as_bytes()
			}
			FileContent::Binary(data) => {
				data
			}
			FileContent::Scene(scene) => {
				temp_text = Some(scene.serialize());
				temp_text.as_ref().unwrap().as_bytes()
			}
			FileContent::Deleted => {
				return Err(std::io::Error::new(std::io::ErrorKind::Other, "Failed to write file"));
			}
		};
		let hash = md5::compute(buf);
		// ensure the directory exists
		if let Some(dir) = path.parent() {
			if !dir.exists() {
				tokio::fs::create_dir_all(dir).await?;
			}
		}
		// Open the file with the appropriate mode
		let mut file = if path.exists() {
			// If file exists, open it for writing (truncate)
			File::options().write(true).truncate(true).open(path)?
		} else {
			// If file doesn't exist, create it
			File::create(path)?
		};
		let result = file.write_all(buf);
		if result.is_err() {
			return Err(std::io::Error::new(std::io::ErrorKind::Other, "Failed to write file"));
		}
		Ok(hash)
	}

	pub async fn write(&self, path: &PathBuf) -> std::io::Result<Digest> {
		FileContent::write_file_content(path, self).await
	}

	// pub fn from_path(path: &PathBuf) -> Option<FileContent> {
	// 	let hash = calculate_file_hash(path);
	// 	if hash.is_none() {
	// 		return FileContent::Binary(std::fs::read(path).unwrap());
	// 	}
	// 	FileContent::String(hash.unwrap())
	// }

	pub fn from_string(string: impl ToString + AsRef<str>) -> FileContent {
		// check if the file is a scene or a tres
		if recognize_scene(string.as_ref()) {
			let scene = parse_scene(string.as_ref());
			if scene.is_ok() {
				return FileContent::Scene(scene.unwrap());
			} else if let Err(e) = scene {
				tracing::error!("Error parsing scene: {:?}", e);
			}

		}
		FileContent::String(string.to_string())
	}

	pub fn from_buf(buf: Vec<u8>) -> FileContent {
		// check the first 8000 bytes (or the entire file if it's less than 8000 bytes) for a null byte
		if is_buf_binary(&buf) {
			return FileContent::Binary(buf);
		}
		let str = str::from_utf8(&buf);
		if str.is_err() {
			return FileContent::Binary(buf);
		}
		let string = str.unwrap();
		FileContent::from_string(string)
	}

	pub fn to_hash(&self) -> Digest {
		match self {
			FileContent::String(s) => md5::compute(s.as_bytes()),
			FileContent::Binary(bytes) => md5::compute(bytes.as_slice()),
			FileContent::Scene(scene) => md5::compute(scene.serialize().as_bytes()),
			FileContent::Deleted => md5::compute(""),
		}
	}

	// NOTE: Probably not appropriate to put here, should have this in BranchState
	pub fn hydrate_content_at(file_entry: ObjId, doc: &Automerge, path: &str, heads: &Vec<ChangeHash>) -> Result<FileContent, Result<DocumentId, io::Error>> {
		let structured_content = doc
		.get_at(&file_entry, "structured_content", heads)
		.unwrap()
		.map(|(value, _)| value);

		if structured_content.is_some() {
			let scene: GodotScene = GodotScene::hydrate_at(doc, path, heads).or_else(|e| {
				tracing::error!("Error hydrating scene: {:?}", e);
				Result::Err(e)
			}).unwrap();
			return Ok(FileContent::Scene(scene));
		}

		if let Ok(Some((_, _))) = doc.get_at(&file_entry, "deleted", &heads) {
			return Ok(FileContent::Deleted);
		}

		// try to read file as text
		let content = doc.get_at(&file_entry, "content", &heads);

		match content {
			Ok(Some((automerge::Value::Object(ObjType::Text), content))) => {
				match doc.text_at(content, &heads) {
					Ok(text) => {
						return Ok(FileContent::String(text.to_string()));
					}
					Err(e) => {
						return Err(Err(io::Error::new(io::ErrorKind::Other, format!("failed to read text file {:?}: {:?}", path, e))));
					}
				}
			}
			_ => match doc.get_string_at(&file_entry, "content", &heads) {
				Some(s) => {
					return Ok(FileContent::String(s.to_string()));
				}
				_ => {
					// return Err(io::Error::new(io::ErrorKind::Other, "Failed to read file"));
				}
			},
		}
		// ... otherwise, check the rul
		let linked_file_content = doc
		.get_string_at(&file_entry, "url", &heads)
		.map(|url| parse_automerge_url(&url)).flatten();
		if linked_file_content.is_some() {
			return Err(Ok(linked_file_content.unwrap()));
		}
		Err(Err(io::Error::new(io::ErrorKind::Other, "Failed to url!")))

	}

	pub fn is_binary(&self) -> bool {
		match self {
			FileContent::Binary(_) => true,
			_ => false,
		}
	}

	pub fn is_text(&self) -> bool {
		match self {
			FileContent::String(_) => true,
			_ => false,
		}
	}

	pub fn is_scene(&self) -> bool {
		match self {
			FileContent::Scene(_) => true,
			_ => false,
		}
	}

	pub fn is_deleted(&self) -> bool {
		match self {
			FileContent::Deleted => true,
			_ => false,
		}
	}

}

impl Default for FileContent {
	fn default() -> Self {
		FileContent::Deleted
	}
}

impl Default for &FileContent {
	fn default() -> Self {
		&FileContent::Deleted
	}
}

pub async fn calculate_file_hash(path: &PathBuf) -> Option<Digest> {
	if !path.is_file() {
		return None;
	}

	let mut file = match tokio::fs::read(path).await {
		Ok(file) => file,
		Err(_) => return None,
	};

	return Some(md5::compute(&mut file));
}

// get the buffer and hash of a file
pub async fn get_buffer_and_hash(path: &PathBuf) -> Result<(Vec<u8>, Digest), tokio::io::Error> {
	if !path.is_file() {
		return Err(io::Error::new(io::ErrorKind::Other, "Not a file"));
	}
	let buf = tokio::fs::read(path).await;
	if buf.is_err() {
		return Err(io::Error::new(io::ErrorKind::Other, "Failed to read file"));
	}
	let buf = buf.unwrap();
	let hash = md5::compute(&buf);
	Ok((buf, hash))
}

pub fn is_buf_binary(buf: &[u8]) -> bool {
	buf.iter().take(8000).filter(|&b| *b == 0).count() > 0
}
