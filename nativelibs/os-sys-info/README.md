# os-sys-info

**English** | [Tiếng Việt](./README.vi.md)

## 📖 Overview

`os-sys-info` is a native Node.js addon for reading operating system and process information. It exposes the operating system architecture and machine labels, and collects process snapshots for a list of process IDs (PIDs). A snapshot includes process identifiers, CPU time counters, memory measurements, and wall-clock and monotonic timestamps. Per-PID lookup failures are returned alongside successful samples so one inaccessible process does not discard other results.

The analyzed native packages target macOS x64, macOS ARM64, Windows x64, and Windows x86. They expose the same three operations: `getOsArchitecture`, `getOsMachine`, and `getProcessSnapshot`.

## 📁 Directory Structure

```text
os-sys-info/
├── src/
│   └── lib.rs                 # Native Rust implementation and Node.js bindings
├── target/
│   └── release/               # Generated release build output
├── Cargo.toml                 # Rust package and dependency configuration
├── README.vi.md               # Vietnamese version of this guide
└── README.md                  # This guide
```

- `src/` contains the Rust library source.
- `target/` contains generated build artifacts; its contents depend on the build configuration.

## 🔎 Technical Analysis

This section describes the observed interface contract of the three exported operations. It documents their inputs and outputs, rather than their internal implementation.

### `getOsArchitecture`

**Input:** None.

**Output:** A string label for the operating-system architecture. The value `64-bit` was observed in the macOS ARM64 package. The exact labels for Windows environments require confirmation on the machine where the package runs; the package binary's architecture alone does not determine the operating system's architecture.

### `getOsMachine`

**Input:** None.

**Output:** A machine code as a string. The observed macOS labels are `x64` and `arm64`. Other values, or cases where the machine cannot be identified, may produce no string result. The label reflects the machine reported by the operating system and does not necessarily match the architecture of the current process.

### `getProcessSnapshot`

Returns an asynchronous result containing process samples and lookup errors.

#### Input

| Property | Rule |
|---|---|
| PID list | Required and must be an array of numbers. A value of the wrong type raises an error with the message `expected pids: number[]`. |
| Each PID | Must be a positive integer smaller than 2³¹. Invalid values are ignored. |
| Duplicate PIDs | Each PID is processed once; the first occurrence determines its position. |
| Empty list or no valid PIDs | Completes immediately with empty `samples` and `errors` arrays. |

#### Output

The completed result contains two arrays:

| Field | Contents |
|---|---|
| `samples` | Successfully read records, with one record for each PID that was retrieved successfully. |
| `errors` | Lookup failure records. Each record contains `pid`, `op` (the failed system operation name), and `code` (the operating-system error code). |

Each item in `samples` has these fields:

| Field | Result type | Meaning / unit |
|---|---|---|
| `pid` | number | Requested process ID. |
| `ppid` | number | Parent process ID. |
| `creation100ns` | high-precision integer | Creation time in the operating system's 100-nanosecond units. |
| `kernelTime100ns` | high-precision integer | Accumulated kernel-mode CPU time in 100-nanosecond units. |
| `userTime100ns` | high-precision integer | Accumulated user-mode CPU time in 100-nanosecond units. |
| `workingSet` | number | Current resident memory in bytes. |
| `privateBytes` | number | Private memory in bytes. |
| `wallUnixMs` | number | Unix wall-clock timestamp in milliseconds. |
| `monotonicMs` | number | Monotonic timestamp in milliseconds, suitable for measuring elapsed time within the same measurement session. |

The CPU time fields are cumulative process counters. CPU usage between two samples can be estimated with:

```text
CPU% = 100 × ((K₂ − K₁) + (U₂ − U₁)) / (T₂ − T₁)
```

Here, `K` is `kernelTime100ns`, `U` is `userTime100ns`, and `T` is elapsed real time converted to the same 100-nanosecond unit. One fully utilized CPU core represents 100%; the result may exceed 100% on a multi-core machine.

#### Result Rules

1. Invalid or duplicate PIDs do not produce entries in `samples` or `errors`.
2. Each valid PID is looked up independently. A successful lookup produces a `sample`; a failed lookup produces a corresponding `error`.
3. An inaccessible PID does not remove successful results for other PIDs in the same request.
4. Successful samples follow the order of unique PIDs in the input. Errors follow PID processing order.
5. Some metrics may be unavailable if the operating system denies access or a process exits during collection. Check `errors` for the failed operation and error code.

## 📝 Compatibility Notes

- The analyzed packages expose the same operation names and field labels. Windows architecture and machine classifications have not been verified by running the packages on Windows; do not infer their return values from the package filename suffix alone.
- This specification describes behavior inferred from static REA analysis. Actual values depend on the operating system, access permissions, and process state at the time of the call.
