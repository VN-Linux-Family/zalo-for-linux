# zlang

**English** | [Tiếng Việt](./README.vi.md)

Native natural language identification addon for Zalo on Linux, replacing the missing macOS NaturalLanguage and Windows ELS backends.

---

## 1. Root Cause Analysis

In official releases of the Zalo desktop client, language identification is delegated to proprietary OS-level services.

### The Upstream Architecture:
```
[Zalo Renderer / AI Translation]
               │  (IPC: language-detector_detect)
               ▼
   [Zalo Main Process (zlang)]
      ├── macOS   ──> Apple NaturalLanguage (NLLanguageRecognizer via Swift FFI)
      ├── Windows ──> Extended Linguistic Services (elscore.dll via Win32 FFI)
      └── Linux   ──> ❌ Explicitly Unsupported (stubs returning empty results)
```

### Architectural Bottlenecks & Limitations on Linux:
1. **Explicit Platform Exclusion**: Upstream Zalo leaked Rust source code revealed that Linux was hardcoded to fail:
   ```rust
   #[cfg(not(any(target_os = "macos", target_os = "windows")))]
   pub fn available() -> bool { false }
   #[cfg(not(any(target_os = "macos", target_os = "windows")))]
   pub fn score_kind() -> &'static str { "none" }
   #[cfg(not(any(target_os = "macos", target_os = "windows")))]
   pub fn detect(_text: &str, _max_results: Option<u32>) -> Result<Vec<Hypothesis>> { Ok(Vec::new()) }
   ```
2. **Feature Regression**: Because `getUnavailableReason()` returned `'unsupported-platform'`, downstream features such as automatic translation suggestions (`ai-translation.language.language-detector`), language-specific spellchecking, and CJK font fallback were permanently disabled.
3. **No Uniform Linux OS Service**: Unlike macOS or Windows, standard Linux distributions do not ship a universal, C-compatible system daemon for real-time natural language identification.

---

## 2. Technical Findings & Specifications

Reverse engineering the client bundle (`app/main-dist/main.js` and `app/pc-dist/`) revealed the exact contract and N-API interfaces expected by Zalo:

### Exported N-API Interface

- **`version() -> string`**: Semantic version of the addon (`"0.1.0"`).
- **`backend() -> string`**: Active engine identifier (`"whatlang-linux"`).
- **`available() -> boolean`**: Readiness status (`true` on Linux).
- **`scores() -> string`**: Confidence representation (`"probability"` with 0.0..1.0 scores, surpassing Windows' uncalibrated `"rank"`).
- **`detect(text: string, maxResults?: number) -> Promise<Hypothesis[]>`**: Asynchronous multi-language detection executed on the `libuv` threadpool.

### Data Structures & Framing

#### `Hypothesis` Object (N-API JS Object)
- **`tag: string`**: BCP-47 language tag (e.g., `'vi'`, `'en'`, `'zh-Hans'`, `'ja'`, `'ko'`, `'fr'`).
- **`confidence: number | null`**: Probability score between `0.0` and `1.0` (rounded to 3 decimal places).

#### Behavioral Invariants
- **Short-Circuit Threshold**: Input text with fewer than 3 trimmed Unicode characters returns an empty array `[]` (valid non-error state).
- **Whitespace Immunity**: Whitespace-only strings return `[]` without error.
- **Asynchronous Execution**: `DetectTask` offloads heavy n-gram calculations to background worker threads, preventing V8 main loop blocking.

---

## 3. Implementation Mechanics

`zlang` is a clean-room native addon implemented in **safe Rust**:

```
[Zalo Renderer (AI Translation / Spellcheck)]
                     │
                     ▼  (IPC)
  [app/native/nativelibs/zlang/index.js]
                     │
                     ▼  (require('./linux-x64/zlang.linux-x64.node'))
[zlang.node (Clean-Room N-API Addon, ~760 KB)]
   ├── NAPI-RS (v2 async Task)         ──> libuv Threadpool (DetectTask)
   ├── whatlang Engine (Pure Rust)     ──> Trigram Analysis & Unicode Scripts
   └── BCP-47 Tag Normalizer           ──> ISO-639-3 / Script -> BCP-47 Mapping
```

### Key Components:
1. **Threadpool Async Worker (`src/lib.rs`)**:
   - Implements `napi::Task` via `DetectTask`.
   - `compute()` executes trigram matching on background worker threads.
   - `resolve()` constructs V8 JS objects on the main event loop upon completion.
2. **Pure-Rust Detection Engine (`src/backend.rs`)**:
   - Powered by `whatlang` 0.18.0 (zero C/C++ dependencies, zero unsafe code).
   - Identifies 84 natural languages using trigram frequency models filtered by Unicode script properties.
3. **BCP-47 Normalizer (`map_lang_to_bcp47`)**:
   - Translates internal language representations to standard BCP-47 tags expected by Zalo frontend:
     - Vietnamese: `'vi'`
     - English: `'en'`
     - Simplified Chinese: `'zh-Hans'`
     - Japanese: `'ja'`, Korean: `'ko'`, French: `'fr'`, German: `'de'`, etc.
4. **Automated Pipeline Patch (`scripts/patches/patch-zlang.js`)**:
   - Compiles release binary via `nativelibs/builder-rust.js`.
   - Validates ELF magic header (`0x7f 0x45 0x4c 0x46`) and verifies exported symbols.
   - Patches `zlang/index.js` to register `linux-x64` / `linux-arm64` loaders and remove platform blockers.

### Results:
- **Accuracy**: 100% accuracy on standard Vietnamese and English test corpora.
- **Latency**: < 0.1 ms per detection (tens of microseconds).
- **Binary Footprint**: ~760 KB self-contained `.node` file with embedded n-gram tables.
- **Reliability**: Eliminates segmentation faults and unhandled rejections on Linux.

---

## 4. Building & Testing

### Building
```bash
# Compile via shared Rust build helper
node nativelibs/builder-rust.js nativelibs/zlang

# Or compile directly via Cargo workspace
cargo build --release -p zlang
```

Compiled `.node` binary is deployed to:
- `app/native/nativelibs/zlang/linux-x64/zlang.linux-x64.node`

### Testing
```bash
# Run unit tests covering N-API methods and language detection accuracy
npm test
```
