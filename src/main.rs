use anyhow::anyhow;
use crossbeam::channel::unbounded;
use crossbeam::channel::Receiver;
use crossbeam::channel::Sender;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::thread;

#[derive(Eq, PartialEq, Debug)]
struct Item {
    id: String,
    json: serde_json::Value,
}

fn check_output(cmd: &mut Command) -> anyhow::Result<String> {
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "cmd exited unsuccessfully: stdout: {} stderr: {}",
            String::from_utf8(output.stdout)?,
            String::from_utf8(output.stderr)?,
        ));
    }

    Ok(String::from_utf8(output.stdout)?)
}

/// Narrow trait representing the subset of functionality from the pp cmdline tool
/// which we need.
///
/// See also: https://1password.com/downloads/command-line/
trait Op: Send + Sync + 'static {
    /// Returns a Vec of ids of items.
    fn list_items(&self) -> anyhow::Result<Vec<String>>;
    fn get_item(&self, id: &str) -> anyhow::Result<serde_json::Value>;
}

struct MockOp {
    // id -> item body result. If the result is absent, it is taken to be
    // a mock error.
    //
    // (The value type in the map is not anyhow::Result<String> because of
    // https://github.com/dtolnay/anyhow/issues/7 preventing cloning the error, so
    // we have to use something else. Until we care about the types of errors in
    // tests, an option is fine.)
    items: HashMap<String, std::option::Option<serde_json::Value>>,
}

impl Op for MockOp {
    fn list_items(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.items.keys().map(|s| s.to_owned()).collect())
    }

    fn get_item(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        match self.items.get(id) {
            Some(body) => match body {
                Some(body) => Ok(body.to_owned()),
                None => Err(anyhow!("mock error for id {}", id)),
            },
            None => Err(anyhow!("no id {} in mock", id)),
        }
    }
}

struct ToolOp {
    /// Command to run (e.g. "op").
    command: String,
    backoff: bool,
}

impl ToolOp {
    fn new(command: String) -> ToolOp {
        ToolOp {
            command,
            backoff: true,
        }
    }

    /// Disable backoff during retries - intended for tests.
    ///
    /// We only disable the backoff, not the retries. We want as much code
    /// coverage as possible, just not actually sleeping.
    #[cfg(test)]
    fn new_without_backoff(command: String) -> ToolOp {
        ToolOp {
            command,
            backoff: false,
        }
    }
}

fn parsed_as_json(s: anyhow::Result<String>) -> anyhow::Result<serde_json::Value> {
    match s {
        Ok(s) => {
            let value = serde_json::from_str::<serde_json::Value>(&s)?;
            Ok(value)
        }
        Err(e) => Err(e),
    }
}

impl Op for ToolOp {
    fn list_items(&self) -> anyhow::Result<Vec<String>> {
        let output = check_output(
            Command::new("/usr/bin/env")
                .arg(self.command.clone())
                .arg("items")
                .arg("list")
                .arg("--format=json"),
        )?;
        let json = serde_json::from_str(&output)?;

        let items = match json {
            serde_json::Value::Array(items) => Ok(items),
            _ => Err(anyhow!(
                "expected JSON list from 'items list', received something else"
            )),
        }?;

        items
            .iter()
            .map(|item| match item {
                serde_json::Value::Object(obj) => {
                    let id = obj.get("id");
                    match id {
                        Some(id) => match id {
                            serde_json::Value::String(id) => Ok(id.into()),
                            _ => Err(anyhow!("item's id key's value is not a string")),
                        },
                        None => Err(anyhow!("item has no id key")),
                    }
                }
                _ => Err(anyhow!("item is not an object")),
            })
            .collect()
    }

    fn get_item(&self, id: &str) -> anyhow::Result<serde_json::Value> {
        let mut tries = 0;

        loop {
            tries += 1;

            let json = parsed_as_json(check_output(
                Command::new("/usr/bin/env")
                    .arg(self.command.clone())
                    .arg("items")
                    .arg("get")
                    .arg("--format=json")
                    .arg(id),
            ));
            match json {
                Ok(json) => return Ok(json),
                Err(e) => {
                    if tries == 5 {
                        return Err(e);
                    }

                    use rand::Rng;
                    let backoff_time =
                        rand::thread_rng().gen_range(tries * 3000, (tries + 1) * 3000);

                    if self.backoff {
                        println!("get item: backing off: {}ms", backoff_time);
                        std::thread::sleep(std::time::Duration::from_millis(backoff_time));
                    } else {
                        println!("get item: would have backed off: {}ms", backoff_time);
                    }
                }
            }
        }
    }
}

struct ProgressReporter {
    last_report: std::time::Instant,
    num_pending: usize,
}

impl ProgressReporter {
    fn new() -> ProgressReporter {
        ProgressReporter {
            last_report: std::time::Instant::now(),
            num_pending: 0,
        }
    }
    fn pending(&mut self) {
        self.num_pending += 1;
    }

    fn done(&mut self) {
        self.num_pending -= 1;

        if self.num_pending > 0 {
            let now = std::time::Instant::now();
            if now.duration_since(self.last_report) > std::time::Duration::from_millis(1000) {
                self.last_report = now;
                eprintln!("{} items still to go", self.num_pending);
            }
        }
    }
}

fn get_items(r: Receiver<String>, s: Sender<anyhow::Result<Item>>, op: Arc<dyn Op>) {
    for id in r {
        if s.send(op.get_item(&id).map(|json| Item { id, json }))
            .is_err()
        {
            break;
        }
    }
}

fn fetch_all_items(op: Arc<dyn Op>) -> anyhow::Result<Vec<Item>> {
    let (id_sender, id_receiver) = unbounded::<String>();
    let (item_sender, item_receiver) = unbounded::<anyhow::Result<Item>>();

    let mut bgthreads: Vec<std::thread::JoinHandle<()>> = vec![];
    for _ in 0..2 {
        let opclone = op.clone();
        let rcvclone = id_receiver.clone();
        let sndclone = item_sender.clone();
        bgthreads.push(thread::spawn(move || {
            get_items(rcvclone, sndclone, opclone);
        }));
    }
    drop(item_sender);

    eprintln!("listing items to export");
    let item_ids = op.list_items()?;

    eprintln!("{} total items - initiating fetch", item_ids.len());

    let mut progress = ProgressReporter::new();
    for id in op.list_items()? {
        progress.pending();
        id_sender.send(id)?;
    }
    drop(id_sender);

    // Note: This pipeline will shortcircuit during collect() if an error is encountered,
    // thus closing the underlying channel since item_receiver will be consumed.
    //
    // Not sure how to make this more explicit while still being idiomatic?
    let items: anyhow::Result<Vec<Item>> = item_receiver
        .into_iter()
        .map(|it| {
            progress.done();
            it
        })
        .collect();

    for thread in bgthreads {
        match thread.join() {
            Ok(_) => (),
            Err(e) => {
                return Err(anyhow!("thread died: {:?}", e));
            }
        }
    }

    items
}

fn id_of_item(item: &serde_json::Value) -> anyhow::Result<String> {
    match item {
        serde_json::Value::Object(obj) => match obj.get("id") {
            Some(id) => match id {
                serde_json::Value::String(id) => Ok(id.to_owned()),
                _ => Err(anyhow!("item's id was not a string")),
            },
            None => Err(anyhow!("item did not have an id")),
        },
        _ => Err(anyhow!("item not an object")),
    }
}

fn export(op_path: &str, dest_path: &str) -> anyhow::Result<()> {
    let tool = ToolOp::new(op_path.to_owned());
    let mut items = fetch_all_items(Arc::new(tool))?;

    items.sort_by_key(|item| id_of_item(&item.json).unwrap());

    let json = serde_json::Value::Array(items.into_iter().map(|it| it.json).collect());
    let pretty_printed = serde_json::to_string_pretty(&json)?;

    std::fs::write(dest_path, pretty_printed)?;

    eprintln!("items written to {} (sorted by id)", dest_path);

    Ok(())
}

fn main() -> anyhow::Result<()> {
    use clap::App;
    use clap::Arg;

    let matches = App::new("op-export")
        .arg(
            Arg::with_name("op")
                .long("op")
                .value_name("PATH")
                .takes_value(true)
                .help("The path of the op binary to use (default: op)."),
        )
        .arg(
            Arg::with_name("output")
                .short("o")
                .long("output")
                .value_name("PATH")
                .takes_value(true)
                .required(true)
                .help("The path to which to write the export in JSON format (required)."),
        )
        .get_matches();

    let op_path = matches.value_of("op").unwrap_or("op");
    let dest_path = matches.value_of("output").unwrap();

    export(op_path, dest_path)?;

    Ok(())
}

#[cfg(test)]
mod test {
    use super::Op;
    use serde_json::json;

    #[test]
    fn test_fetch_all_items_all_no_items() -> anyhow::Result<()> {
        let items = super::fetch_all_items(std::sync::Arc::new(super::MockOp {
            items: std::collections::HashMap::new(),
        }))?;

        assert_eq!(0, items.len());

        Ok(())
    }

    #[test]
    fn test_fetch_all_items_all_success() -> anyhow::Result<()> {
        let items = super::fetch_all_items(std::sync::Arc::new(super::MockOp {
            items: vec![
                ("id1".to_owned(), Some(json!({"id": "id1"}))),
                ("id2".to_owned(), Some(json!({"id": "id2"}))),
            ]
            .into_iter()
            .collect(),
        }))?;

        let items: std::collections::HashMap<String, super::Item> =
            items.into_iter().map(|it| (it.id.clone(), it)).collect();

        assert_eq!(2, items.len());
        assert_eq!(
            super::Item {
                id: "id1".into(),
                json: json!({"id": "id1"}),
            },
            *items.get("id1").unwrap()
        );
        assert_eq!(
            super::Item {
                id: "id2".into(),
                json: json!({"id": "id2"}),
            },
            *items.get("id2").unwrap()
        );

        Ok(())
    }

    #[test]
    fn test_fetch_all_items_some_failed() -> anyhow::Result<()> {
        let items = super::fetch_all_items(std::sync::Arc::new(super::MockOp {
            items: vec![
                ("id1".to_owned(), Some(json!({"id": "id1"}))),
                ("id2".to_owned(), None),
                ("id3".to_owned(), Some(json!({"id": "id3"}))),
            ]
            .into_iter()
            .collect(),
        }));

        match items {
            Ok(_) => Err(anyhow::anyhow!("fetch should have failed")),
            Err(e) => {
                assert_eq!("mock error for id id2", e.to_string());
                Ok(())
            }
        }
    }

    struct MockTool {
        path: tempfile::TempPath,
    }

    impl MockTool {
        fn new(body: &[u8]) -> MockTool {
            use std::io::prelude::*;

            let mut file = tempfile::NamedTempFile::new().unwrap();

            file.as_file_mut().write_all(&body).unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(file.path()).unwrap().permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(file.path(), perms).unwrap();

            // Turning it into a TempPath is important. On Linux (but not MacOS), we
            // must close the underlying file before there is an attempt to execute
            // it - or executation will fail with "text file busy".
            MockTool {
                path: file.into_temp_path(),
            }
        }
    }

    fn optool(tool_body: &[u8]) -> (super::ToolOp, MockTool) {
        let tool = MockTool::new(tool_body);
        let op = super::ToolOp::new_without_backoff(tool.path.to_str().unwrap().into());

        (op, tool)
    }

    #[test]
    fn test_tool_op_list_items_empty() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n echo '[]'");

        let items = op.list_items().unwrap();
        assert_eq!(0, items.len());

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_one_item() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n echo '[{\"id\": \"value\"}]'");

        let items = op.list_items().unwrap();
        assert_eq!(1, items.len());
        assert_eq!("value", items.get(0).unwrap());

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_two_items() -> anyhow::Result<()> {
        let (op, _tool) =
            optool(b"#!/bin/bash\n echo '[{\"id\": \"value1\"}, {\"id\": \"value2\"}]'");

        let items = op.list_items().unwrap();
        assert_eq!(2, items.len());
        assert_eq!("value1", items.get(0).unwrap());
        assert_eq!("value2", items.get(1).unwrap());

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_not_json() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n echo 'this is not json'");

        // XXX: should look for specific error
        assert!(op.list_items().is_err());

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_exit1() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\nexit 1");

        // XXX: should look for specific error
        assert!(op.list_items().is_err());

        Ok(())
    }

    #[test]
    fn test_tool_op_get_item_success() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n echo '{\"key\": \"value\"}'");

        let item = op.get_item("id").unwrap();
        assert_eq!(serde_json::json!({"key": "value"}), item);

        Ok(())
    }

    #[test]
    fn test_tool_op_get_item_exit1() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\nexit 1");

        // XXX: should look for specific error
        assert!(op.get_item("id").is_err());

        Ok(())
    }

    #[test]
    fn test_tool_op_get_item_correct_arguments() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n [[ \"$1\" == \"items\" ]] && [[ \"$2\" == \"get\" ]] && [[ \"$3\" == \"--format=json\" ]] && [[ \"$4\" == \"id\" ]] && [[ \"$5\" == \"\" ]] && echo {}");

        op.get_item("id").unwrap();

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_correct_arguments() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n [[ \"$1\" == \"items\" ]] && [[ \"$2\" == \"list\" ]] && [[ \"$3\" == \"--format=json\" ]] && [[ \"$4\" == \"\" ]] && echo \"[]\"");

        op.list_items().unwrap();

        Ok(())
    }

    #[test]
    fn test_tool_op_get_item_kill9() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n kill -9 $$");

        assert!(op.get_item("id").is_err());

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_kill9() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n kill -9 $$");

        assert!(op.list_items().is_err());

        Ok(())
    }
}
