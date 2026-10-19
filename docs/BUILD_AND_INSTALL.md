# Build and install

## Host build and tests

```sh
cargo test --workspace              # unit + fixture + end-to-end daemon tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Rust 1.85 or newer (MSRV, checked in CI); CI uses 1.97.0. Tests never touch the host's
`/sys` or `/proc`: every platform access goes through a temporary fixture root, and
`dumpsys` is faked.

## Android module

Requirements: the `aarch64-linux-android` Rust target and an Android NDK (CI uses r27d,
SHA-256 `601246087a682d1944e1e16dd85bc6e49560fe8b6d61255be2829178c8ed15d9`).

```sh
rustup target add aarch64-linux-android
export ANDROID_NDK_HOME=/path/to/android-ndk-r27d
scripts/build-android.sh
# -> dist/zairenkai-fas-v<version>.zip and .sha256
```

The binary links against API level 29 (Android 10) and uses only bionic libc. The zip has fixed
timestamps and file order.

## Install

1. Flash `dist/zairenkai-fas-v*.zip` from the Magisk, KernelSU or APatch app.
   `customize.sh` refuses non-arm64 devices and API levels below 29, and prints the
   capability probe.
2. Reboot. `service.sh` waits for `sys.boot_completed`, then supervises `zfasd run`.
3. Check the status:
   ```sh
   su -c /data/adb/modules/zairenkai_fas/bin/zfasd ctl status
   ```
   On KernelSU, the module's WebUI shows the same information.

Try **observe mode first** on a new device: set `general.mode = "observe"` in
`/data/adb/zairenkai-fas/config.toml`. Then watch `state.json` and `zfasd.log` while playing
to see what the controller *would* do, before allowing writes.

## Remove or recover

- Disable the module in the manager, or run `zfasd ctl disable`. Both release all floors.
- Uninstalling runs `zfasd restore` and deletes `/data/adb/zairenkai-fas`.
- Manual recovery: `su -c /data/adb/modules/zairenkai_fas/bin/zfasd restore`.
- A reboot always resets cpufreq floors to kernel defaults; nothing is persisted.
