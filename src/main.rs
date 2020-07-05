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
    uuid: String,
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
    /// Returns a Vec of uuids of items.
    fn list_items(&self) -> anyhow::Result<Vec<String>>;
    fn get_item(&self, uuid: &str) -> anyhow::Result<serde_json::Value>;
}

struct MockOp {
    // uuid -> item body result. If the result is absent, it is taken to be
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

    fn get_item(&self, uuid: &str) -> anyhow::Result<serde_json::Value> {
        match self.items.get(uuid) {
            Some(body) => match body {
                Some(body) => Ok(body.to_owned()),
                None => Err(anyhow!("mock error for uuid {}", uuid)),
            },
            None => Err(anyhow!("no uuid {} in mock", uuid)),
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
                .arg("list")
                .arg("items"),
        )?;
        let json = serde_json::from_str(&output)?;

        let items = match json {
            serde_json::Value::Array(items) => Ok(items),
            _ => Err(anyhow!(
                "expected JSON list from 'list items', received something else"
            )),
        }?;

        items
            .iter()
            .map(|item| match item {
                serde_json::Value::Object(obj) => {
                    let uuid = obj.get("uuid");
                    match uuid {
                        Some(uuid) => match uuid {
                            serde_json::Value::String(uuid) => Ok(uuid.into()),
                            _ => Err(anyhow!("item's uuid key's value is not a string")),
                        },
                        None => Err(anyhow!("item has no uuid key")),
                    }
                }
                _ => Err(anyhow!("item is not an object")),
            })
            .collect()
    }

    fn get_item(&self, uuid: &str) -> anyhow::Result<serde_json::Value> {
        let mut tries = 0;

        loop {
            tries += 1;

            let json = parsed_as_json(check_output(
                Command::new("/usr/bin/env")
                    .arg(self.command.clone())
                    .arg("get")
                    .arg("item")
                    .arg(uuid),
            ));
            match json {
                Ok(json) => return Ok(json),
                Err(e) => {
                    if tries == 5 {
                        return Err(e);
                    }

                    use rand::Rng;
                    let backoff_time =
                        rand::thread_rng().gen_range(tries * 1000, (tries + 1) * 1000);

                    if self.backoff {
                        println!("backing off: {}ms", backoff_time);
                        std::thread::sleep(std::time::Duration::from_millis(backoff_time));
                    } else {
                        println!("would have backed off: {}ms", backoff_time);
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
    for uuid in r {
        if s.send(op.get_item(&uuid).map(|json| Item { uuid, json }))
            .is_err()
        {
            break;
        }
    }
}

fn fetch_all_items(op: Arc<dyn Op>) -> anyhow::Result<Vec<Item>> {
    let (uuid_sender, uuid_receiver) = unbounded::<String>();
    let (item_sender, item_receiver) = unbounded::<anyhow::Result<Item>>();

    let mut bgthreads: Vec<std::thread::JoinHandle<()>> = vec![];
    for _ in 0..5 {
        let opclone = op.clone();
        let rcvclone = uuid_receiver.clone();
        let sndclone = item_sender.clone();
        bgthreads.push(thread::spawn(move || {
            get_items(rcvclone, sndclone, opclone);
        }));
    }
    drop(item_sender);

    let mut progress = ProgressReporter::new();
    for uuid in op.list_items()? {
        progress.pending();
        uuid_sender.send(uuid)?;
    }
    drop(uuid_sender);

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

fn main() -> anyhow::Result<()> {
    let tool = ToolOp::new("op".into());
    let items = fetch_all_items(Arc::new(tool))?;

    for item in items {
        println!(
            "{}: {}",
            item.uuid,
            serde_json::to_string_pretty(&item.json).unwrap(),
        );
    }

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
                ("uuid1".to_owned(), Some(json!({"uuid": "uuid1"}))),
                ("uuid2".to_owned(), Some(json!({"uuid": "uuid2"}))),
            ]
            .into_iter()
            .collect(),
        }))?;

        let items: std::collections::HashMap<String, super::Item> =
            items.into_iter().map(|it| (it.uuid.clone(), it)).collect();

        assert_eq!(2, items.len());
        assert_eq!(
            super::Item {
                uuid: "uuid1".into(),
                json: json!({"uuid": "uuid1"}),
            },
            *items.get("uuid1").unwrap()
        );
        assert_eq!(
            super::Item {
                uuid: "uuid2".into(),
                json: json!({"uuid": "uuid2"}),
            },
            *items.get("uuid2").unwrap()
        );

        Ok(())
    }

    #[test]
    fn test_fetch_all_items_some_failed() -> anyhow::Result<()> {
        let items = super::fetch_all_items(std::sync::Arc::new(super::MockOp {
            items: vec![
                ("uuid1".to_owned(), Some(json!({"uuid": "uuid1"}))),
                ("uuid2".to_owned(), None),
                ("uuid3".to_owned(), Some(json!({"uuid": "uuid3"}))),
            ]
            .into_iter()
            .collect(),
        }));

        match items {
            Ok(_) => Err(anyhow::anyhow!("fetch should have failed")),
            Err(e) => {
                assert_eq!("mock error for uuid uuid2", e.to_string());
                Ok(())
            }
        }
    }

    struct MockTool {
        file: tempfile::NamedTempFile,
    }

    impl MockTool {
        fn new(body: &[u8]) -> MockTool {
            let mut tool = MockTool {
                file: tempfile::NamedTempFile::new().unwrap(),
            };
            use std::io::prelude::*;
            tool.file.as_file_mut().write_all(&body).unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(tool.file.path()).unwrap().permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(tool.file.path(), perms).unwrap();
            tool
        }
    }

    fn optool(tool_body: &[u8]) -> (super::ToolOp, MockTool) {
        let tool = MockTool::new(tool_body);
        let op = super::ToolOp::new_without_backoff(tool.file.path().to_str().unwrap().into());

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
        let (op, _tool) = optool(b"#!/bin/bash\n echo '[{\"uuid\": \"value\"}]'");

        let items = op.list_items().unwrap();
        assert_eq!(1, items.len());
        assert_eq!("value", items.get(0).unwrap());

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_two_items() -> anyhow::Result<()> {
        let (op, _tool) =
            optool(b"#!/bin/bash\n echo '[{\"uuid\": \"value1\"}, {\"uuid\": \"value2\"}]'");

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

        let item = op.get_item("uuid").unwrap();
        assert_eq!(serde_json::json!({"key": "value"}), item);

        Ok(())
    }

    #[test]
    fn test_tool_op_get_item_exit1() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\nexit 1");

        // XXX: should look for specific error
        assert!(op.get_item("uuid").is_err());

        Ok(())
    }

    #[test]
    fn test_tool_op_get_item_correct_arguments() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n [[ \"$1\" == \"get\" ]] && [[ \"$2\" == \"item\" ]] && [[ \"$3\" == \"uuid\" ]] && [[ \"$4\" == \"\" ]] && echo {}");

        op.get_item("uuid").unwrap();

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_correct_arguments() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n [[ \"$1\" == \"list\" ]] && [[ \"$2\" == \"items\" ]] && [[ \"$3\" == \"\" ]] && echo \"[]\"");

        op.list_items().unwrap();

        Ok(())
    }

    #[test]
    fn test_tool_op_get_item_kill9() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n kill -9 $$");

        assert!(op.get_item("uuid").is_err());

        Ok(())
    }

    #[test]
    fn test_tool_op_list_items_kill9() -> anyhow::Result<()> {
        let (op, _tool) = optool(b"#!/bin/bash\n kill -9 $$");

        assert!(op.list_items().is_err());

        Ok(())
    }
}
