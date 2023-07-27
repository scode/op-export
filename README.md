# op-export

> [!WARNING]  
> YOU PROBABLY SHOULD NOT USE THIS TOOL. Please read the README in
> full before considering using it.

op-export is a command line tool allowing you to export (or backup)
items in a [1Password](https://1password.com/) vault.

## First, you probably SHOULD NOT USE THIS TOOL

Prior to deciding to use op-export, it is recommendted to first
evaluate the backup methods [offered by 1Password
itself](https://support.1password.com/backups/) to see if those
satisfy your requirements.

In particular, note that the native 1Password app allows exporting
into [an unencrypted
format](https://support.1password.com/1pux-format/) which is easy to
consume as a human or in tools (JSON based).

## Limitations

* This tool has only been tested for basic login information
  (username/password). You should manually expect the output
  to determine it has the information you expect. For example,
  "document" items in 1password do not contain file contents
  inline in the item JSON returned through the command line
  tool. Those will not be backed up.

## If you really want to use it anyway

The output is intentionally *unencrypted*. You may wish to combine
with [saltybox](https://github.com/scode/saltybox) to get an offline
encrypted backup of your vault that is independent of the 1Password
service and software.

Please note that there is currently no corresponding ability to
*import* data back into 1Password. The tool is intended as a last
resort emergency backup to prevent total loss of vault data; ability
to import was not a major concern for its intended use case.

## Installation

* Install `op`, the official command line tool [provided by 1Password](https://1password.com/downloads/command-line/).
  At the time of this writing, version 2.18.0 has been tested.
* Install rust by following the instructions at https://rustup.rs/
* Install by running `cargo install --path .`

## Usage

Sign into 1password using `op`:

```
eval $(op signin)
```

Export all items in your vault to `~/tmp/1p.json` (will *not be encrypted*):

```
op-export -o ~/tmp/1p.json
```

Although `op-export` tries hard to let you know if anything goes
wrong, it is recommended that the resulting file be inspected to
ensure it looks reasonable.

`op-export` operates by invoking `op list items` followed by using `op
get item` to fetch each item listed. If `op get item` gives us valid
JSON, we assume success and accept the JSON as a valid vault item.

## Troubleshooting

The `op` tool has various strange failure modes, often different type
of parsing errors rather than telling you what is actually wrong. Some
modes of failure I have found require you to `op signin` again to
unwedge it.
