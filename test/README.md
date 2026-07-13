# NCD application TUI

[`ncd_tui.py`](./ncd_tui.py) is only the Linux application layer. It does not modify or replace `ncdd`, `ncd`, the kernel driver, or any adapter. The data path remains:

```text
TUI <-> /dev/ncd_* <-> driver <-> ncdd <-> network <-> ncd <-> adapter
```

## Start and select a connection

The TUI reads every `[[device]]` entry from `/etc/ncd/config.toml` in file order. It does not invent or open a default connection.

```bash
python3 test/ncd_tui.py
```

On the home page, use one simple selector:

```text
control+camera
control+ncd_keyboard
1
```

The selected page opens its connection automatically. `back` closes it automatically. Within that page, `close`, `open`, and `reopen` may be repeated. Common commands are `status`, `help`, `back`, and `quit`.

All run data is saved below `test/runs/<UTC time>/`. Every page prints its exact directory.

## Keyboard

The prompt is `linux->device:`. Commands mean:

```text
send hello world       type this text in the focused application
send+hello world       same operation, compact form
enter                  tap Enter
key left               tap the Left Arrow key
combo ctrl+c           hold Ctrl, tap C, release Ctrl
combo shift+a          hold Shift, tap A, release Shift
combo command+s        hold Command/Windows, tap S, release it
down shift             hold Shift
up shift               release Shift
raw                     direct input mode; Ctrl-] exits
listen text             display received press events as readable text/keys
listen events           display complete received JSON events
listen off              hide events but continue receiving and saving them
info                    show receive/send log paths
```

Received input is shown immediately as `device->linux: ...`. Complete events are durably appended to `events.jsonl`; reconstructed text goes to `text.txt`; sent commands go to `commands.jsonl`.

The existing keyboard adapter has no command ACK. Therefore `written to NCD` means the bytes reached the NCD character-device path; it does not prove the target application accepted the key. On the actual device, the target input box must have focus and the OS must permit `pynput` input injection/listening.

## Camera

Camera receiving is continuous while the page is open. Every complete image is atomically published as a timestamped `.jpg` and as `latest.jpg`; metadata is durably appended to `frames.jsonl`. Filenames and displayed times use UTC (`...Z`) to avoid ambiguous or incorrect local timestamps.

```text
status                 receive/save health, frame count, UTC save time and interval
latest                 print the latest saved path
latest copy.jpg        copy the latest saved frame
capture [copy.jpg]     wait for the next frame
path                   show image and metadata paths
```

To keep the terminal readable, only the first frame and then one status sample about every five seconds are displayed. The sample is a short hexadecimal prefix, never the whole image. Disk saving does not wait for the display-status queue.

`STALE (possible disconnect)` means no frame has arrived recently. With the current blocking character-device interface, an idle connection and a dead peer cannot always be distinguished without changing the driver/ncdd contract; the TUI reports this limitation instead of claiming the connection is healthy.

## File

The current FileAdapter sends a full snapshot when it opens and whenever the mapped file changes. The TUI always saves those snapshots.

```text
read                   show up to 4096 bytes from the latest snapshot
write hello            append text and wait for a returned snapshot
write+hello            same operation, compact form
save [OUTPUT]          copy the complete latest snapshot
writefile PATH         append a local Linux file
info                   show size, hash, UTC receive time and paths
reopen                 require a new initial snapshot
```

A write is reported as confirmed only when a newer snapshot is returned and ends with the exact payload.

### Required device-side file configuration

The error `file_path option is required for FileAdapter` means the Windows/actual-device `ncd` configuration is missing the adapter option. It cannot be supplied later by the Linux TUI because the adapter fails before the connection protocol starts. Configure the existing adapter on the device side, for example:

```toml
[[device]]
driver = "file"
device_identifier = "Unspecified"
device_name = "File Device"
port = 8000
options = { file_path = "C:\\Users\\Lenovo\\ncd-share.txt", poll_interval_ms = "200" }
```

Then restart device-side `ncd` and use `reopen` (or re-enter the file page). The TUI now emits a direct `file_path` diagnosis if no initial snapshot arrives.

## Instruction

The prompt is `linux->device:`; each matched response is shown as `device->linux:` with `ok`, exit code, system, stdout, and stderr.

```text
exec whoami             run one real executable without a shell
win dir                 Windows: cmd.exe /d /s /c "dir"
win echo 12345          Windows command without requiring allow_shell
unix uname -a           Unix: /bin/sh -lc "uname -a"
shell echo hello        advanced shell=True mode; requires allow_shell=true
timeout 10000           set adapter request timeout in milliseconds
info                    show request/response/stdout/stderr logs
```

`PROGRAM` in old help was a placeholder; entering `exec PROGRAM ...` cannot work. `win` and `unix` use an argv request, so they avoid the adapter's default `allow_shell=false`. The raw `shell` command intentionally remains subject to that security option. A missing response has a real TUI timeout and is reported as a possible disconnect.

## Verification

```bash
python3 -m unittest discover -s test -p 'test_*.py' -v
```

The tests cover connection selection, durable camera saves and UTC metadata, keyboard bidirectional events and combinations, instruction request IDs/Windows argv/timeouts, file snapshot confirmation and missing-`file_path` diagnosis, actual existing adapter round trips, and repeated page-local open/close.
