# db-cross-v4

Linux reimplementation of Zalo's macOS `db-cross-v4-native.node` addon, used
for backup decryption and message synchronization.

## Origin

This is a clean-room reimplementation in pure Rust (NAPI-RS), based on the open wire and container specifications documented by the [alolite project](https://github.com/ngxccc/alolite) (licensed under MIT / Apache-2.0).

It provides complete wire and contract parity with the official Zalo macOS `db-cross-v4-native.node` binary without using any proprietary code, legacy C++ code, or external system C headers.

## Architecture

- **`crypto`**: AES-256 key derivation from ASCII passphrase (`derive_aes256_key_zalo_v2`) and chunked 64 KiB AES-256-CBC decryption with zero IV reset.
- **`container`**: `ZDB4.0` backup archive container unpacker, LZMA2/XZ stream extraction with `YZ` footer boundary detection, Path Traversal protection, and SQLite header validation.
- **`tlv`**: Little-Endian / Big-Endian heuristic scoring TLV parser with smart UTF-16LE decoding.
- **`binnet`**: Structured parser for `ChatContent.BinNet` (attachments `0x06`, quotes `0x07`, mentions `0x08`, and properties `0x05`).
- **`lib`**: N-API exports for `decompressAndDecryptDb_V2`, `parseBinNet`, and `decompressAndDecryptDb`.

## Requirements

Pure Rust build: requires only the Rust toolchain (stable). Zero C++ compilers, zero OpenSSL headers, zero liblzma headers.

## Credits & Attribution

- **Implementation**: Clean-room reverse engineering and native Linux implementation by [@ngxccc](https://github.com/ngxccc) ([alolite](https://github.com/ngxccc/alolite)).
- **Integration**: Maintained by [Việt Nam Linux Family](https://github.com/VN-Linux-Family) under the BSD-3-Clause license.
