# Running Python Scripts in OpenAB

OpenAB Docker images do **not** ship a system Python or `uv`. This is by design — images are kept minimal for simplicity, and users are free to install anything they need into `$HOME`, which is fully persisted across restarts. The recommended way to run Python scripts is with [`uv`](https://docs.astral.sh/uv/).

## Installing `uv`

On first use, the AI agent will typically install `uv` automatically. You can also install it manually:

```bash
curl -LsSf https://astral.sh/uv/install.sh | sh
```

This installs `uv` to `~/.local/bin/`.

To ensure `uv` is always available before the agent starts, add the install command to `hooks.pre_boot` in your `config.toml`:

```toml
[hooks]
pre_boot = ["curl -LsSf https://astral.sh/uv/install.sh | sh"]
```

See [Hooks](hooks.md) for details.

Once installed, `uv run` handles everything else — downloading Python, managing dependencies, and executing your script.

> **Note:** In OAB deployments, the entire `$HOME` directory is persisted (via PVC or volume mount). This means `uv` only downloads and installs a managed Python on the **first run** — subsequent runs reuse the cached interpreter from `~/.local/share/uv/python/`.

## Quick Start

```bash
uv run script.py
```

`uv run` automatically:

1. Detects the required Python version (from `.python-version` or `pyproject.toml`)
2. Downloads and installs a managed Python if none is available
3. Creates a virtual environment and installs dependencies
4. Runs your script

No manual `python` install needed.

## Pin a Python Version

Create a `.python-version` file in your working directory:

```bash
uv python pin 3.12
```

This writes `.python-version` with `3.12`, ensuring all `uv run` invocations use that version.

## Scripts with Dependencies

For a standalone script that needs packages, add inline metadata:

```python
# /// script
# requires-python = ">=3.11"
# dependencies = ["requests", "beautifulsoup4"]
# ///

import requests
from bs4 import BeautifulSoup

resp = requests.get("https://example.com")
soup = BeautifulSoup(resp.text, "html.parser")
print(soup.title.string)
```

Then simply:

```bash
uv run script.py
```

`uv` resolves and installs the declared dependencies automatically.

Alternatively, pass dependencies on the command line:

```bash
uv run --with requests --with beautifulsoup4 script.py
```

## Project with `pyproject.toml`

For multi-file projects, use a standard `pyproject.toml`:

```toml
[project]
name = "my-tool"
version = "0.1.0"
requires-python = ">=3.11"
dependencies = ["httpx"]
```

Then run any module:

```bash
uv run python -m my_tool
```

`uv` syncs the environment automatically before execution.

## How `uv run` Finds Python

Resolution order:

1. `.python-version` file (project root or parents)
2. Existing virtual environment (`.venv` or `VIRTUAL_ENV`)
3. `requires-python` in `pyproject.toml`
4. uv-managed installs (`~/.local/share/uv/python/`)
5. System `PATH` fallback

If no suitable interpreter exists, `uv` downloads one automatically.

## Example: Skill with Python Scripts

A typical skill directory structure:

```
twitter/
├── scripts/
│   ├── collect_timeline.py
│   ├── collect_comments.py
│   ├── filter_candidates.py
│   └── upload.py
└── SKILL.md
```

In your `SKILL.md`, instruct the agent to run scripts using `uv run` with the skill directory as the base path:

```markdown
## Execution

- **SKILL_DIR** = the directory containing this SKILL.md file.
  Resolve all `scripts/` paths relative to it.

To upload, run:

    uv run ${SKILL_DIR}/scripts/upload.py

To collect timeline:

    uv run ${SKILL_DIR}/scripts/collect_timeline.py
```

The agent resolves `${SKILL_DIR}` to the actual skill path (e.g. `~/.kiro/skills/twitter/`) and runs the script directly. Each Python script can declare its own dependencies inline:

```python
# /// script
# requires-python = ">=3.11"
# dependencies = ["tweepy", "httpx"]
# ///

import tweepy
# ... skill logic
```

No virtualenv setup, no `pip install` — `uv run` handles everything.

## Tips

- **First run is slower** — Python download + dependency install is cached for subsequent runs.
- **Offline environments** — Pre-install Python with `uv python install 3.12` during image build if network is unavailable at runtime.
- **Force a version** — `uv run --python 3.13 script.py` overrides all resolution.
- **See installed Pythons** — `uv python list` shows all available interpreters.
