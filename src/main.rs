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
    body: String,
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
    fn get_item(&self, uuid: &str) -> anyhow::Result<String>;
}

struct MockOp {
    // uuid -> item body
    items: HashMap<String, String>,
}

impl Op for MockOp {
    fn list_items(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.items.keys().map(|s| s.to_owned()).collect())
    }

    fn get_item(&self, uuid: &str) -> anyhow::Result<String> {
        match self.items.get(uuid) {
            Some(body) => Ok(body.to_owned()),
            None => Err(anyhow!("no item by this uuid: {}", uuid)),
        }
    }
}

struct ToolOp {
    /// Command to run (e.g. "op").
    command: String,
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

    fn get_item(&self, uuid: &str) -> anyhow::Result<String> {
        let output = check_output(
            Command::new("/usr/bin/env")
                .arg(self.command.clone())
                .arg("get")
                .arg("item")
                .arg(uuid),
        )?;

        Ok(output)
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
        self.num_pending -= 0;

        let now = std::time::Instant::now();
        if now.duration_since(self.last_report) > std::time::Duration::from_millis(1000) {
            self.last_report = now;
            eprintln!("{} items still to go", self.num_pending);
        }
    }
}

fn get_items(r: Receiver<String>, s: Sender<anyhow::Result<Item>>, op: Arc<dyn Op>) {
    for uuid in r {
        s.send(op.get_item(&uuid).map(|body| Item { uuid, body }))
            .unwrap();
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
    for uuid in op.list_items().unwrap() {
        progress.pending();
        uuid_sender.send(uuid).unwrap();
    }
    drop(uuid_sender);

    let items: anyhow::Result<Vec<Item>> = item_receiver
        .into_iter()
        .map(|it| {
            progress.done();
            it
        })
        .collect();

    for thread in bgthreads {
        thread.join().unwrap();
    }

    items
}

fn main() -> anyhow::Result<()> {
    let items = fetch_all_items(Arc::new(MockOp {
        items: [
            ("uuid1".to_owned(), "1body".to_owned()),
            ("uuid2".to_owned(), "2body".to_owned()),
        ]
        .iter()
        .cloned()
        .collect(),
    }))?;

    for item in items {
        println!("{}: {}", item.uuid, item.body);
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use super::Op;

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
                ("uuid1".to_owned(), "1body".to_owned()),
                ("uuid2".to_owned(), "2body".to_owned()),
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
                body: "1body".into(),
            },
            *items.get("uuid1").unwrap()
        );
        assert_eq!(
            super::Item {
                uuid: "uuid2".into(),
                body: "2body".into()
            },
            *items.get("uuid2").unwrap()
        );

        Ok(())
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
        let op = super::ToolOp {
            command: tool.file.path().to_str().unwrap().into(),
        };

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
        let item_json: serde_json::Value = serde_json::from_str(&item)?;
        assert_eq!(serde_json::json!({"key": "value"}), item_json);

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
