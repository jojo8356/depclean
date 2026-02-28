<p align="center">
  <h1 align="center">🧹 depclean</h1>
  <p align="center">
    <strong>Interactive TUI tool to reclaim disk space by cleaning project dependencies</strong>
  </p>
  <p align="center">
    <a href="https://github.com/jojo8356/depclean/actions"><img src="https://github.com/jojo8356/depclean/actions/workflows/release.yml/badge.svg" alt="Build"></a>
    <a href="https://github.com/jojo8356/depclean/releases/latest"><img src="https://img.shields.io/github/v/release/jojo8356/depclean?color=blue" alt="Release"></a>
    <a href="https://github.com/jojo8356/depclean/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-green" alt="License"></a>
  </p>
</p>

<br>

Scan your filesystem, visualize how much space each project's dependencies consume, and selectively delete them — all from a beautiful terminal interface.

Built in Rust for speed. Single binary, zero runtime dependencies, **~650 KB**.

<br>

## Supported Languages

| Language | Project Marker | Cleaned Directories |
|----------|---------------|-------------------|
| Rust | `Cargo.toml` | `target/` |
| Node.js | `package.json` | `node_modules/` |
| Python | `pyproject.toml`, `setup.py`, `requirements.txt` | `venv/`, `.venv/`, `__pycache__/` |
| Java / Kotlin | `build.gradle`, `pom.xml` | `build/`, `.gradle/`, `target/` |
| Go | `go.mod` | `vendor/` |
| C / C++ | `CMakeLists.txt` | `build/` |
| .NET | `*.csproj`, `*.sln` | `bin/`, `obj/` |

<br>

## Installation

### From releases (recommended)

Download the latest binary for your platform from the [Releases page](https://github.com/jojo8356/depclean/releases/latest).

```bash
# Linux (amd64)
curl -L https://github.com/jojo8356/depclean/releases/latest/download/depclean-linux-amd64 -o depclean
chmod +x depclean
sudo mv depclean /usr/local/bin/

# macOS (Apple Silicon)
curl -L https://github.com/jojo8356/depclean/releases/latest/download/depclean-macos-arm64 -o depclean
chmod +x depclean
sudo mv depclean /usr/local/bin/
```

### From source

```bash
git clone https://github.com/jojo8356/depclean.git
cd depclean
cargo build --release
cp target/release/depclean ~/.local/bin/
```

<br>

## Usage

```bash
depclean              # scan current directory
depclean ~            # scan entire home directory
depclean ~/projects   # scan a specific directory
```

<br>

## Keybindings

| Key | Action |
|-----|--------|
| `↑` `↓` or `k` `j` | Navigate project list |
| `Space` | Toggle selection |
| `a` | Select / deselect all |
| `Enter` | Delete selected dependencies |
| `y` / `n` | Confirm / cancel deletion |
| `q` or `Esc` | Quit |

<br>

## How It Works

1. **Scan** — Recursively walks the target directory, detecting projects by their marker files
2. **Analyze** — Calculates the size of each dependency directory
3. **Display** — Presents projects sorted by size (largest first) in an interactive table
4. **Clean** — Deletes selected dependency directories after explicit confirmation

Dependencies can always be restored with `cargo build`, `npm install`, `pip install`, etc.

<br>

## Platforms

| Platform | Architecture | Binary |
|----------|-------------|--------|
| Linux | x86_64 | `depclean-linux-amd64` |
| Linux | ARM64 | `depclean-linux-arm64` |
| macOS | x86_64 (Intel) | `depclean-macos-amd64` |
| macOS | ARM64 (Apple Silicon) | `depclean-macos-arm64` |
| Windows | x86_64 | `depclean-windows-amd64.exe` |

<br>

## License

MIT — see [LICENSE](LICENSE) for details.
