.PHONY: test lint actionlint format typecheck check build-worker run-worker rustfmt clippy rust-test smoke-worker smoke-base smoke-long-note smoke-light-theme smoke-obsidian smoke-render-markdown smoke-render-markdown-rich smoke-jupynvim smoke-doctor smoke-health smoke-help smoke-integrations smoke-repros

test:
	nvim -l tests/minit.lua --minitest $(FILE)

lint:
	stylua --check lua/ plugin/ repro/ tests/

ACTIONLINT_VERSION := v1.7.12
ACTIONLINT ?= go run github.com/rhysd/actionlint/cmd/actionlint@$(ACTIONLINT_VERSION)
actionlint:
	$(ACTIONLINT)

format:
	stylua lua/ plugin/ repro/ tests/
	cargo fmt

NVIM_VIMRUNTIME := $(shell nvim --headless -c 'lua io.write(vim.env.VIMRUNTIME)' -c 'q' 2>&1)
typecheck:
	VIM="$(NVIM_VIMRUNTIME)/.." lua-language-server --check_format=pretty --check lua/ --checklevel=Warning --configpath="$$(pwd)/.luarc.json"

build-worker:
	cargo build --locked --release --package render-latex-worker

run-worker:
	cargo run --locked --release --package render-latex-worker

rustfmt:
	cargo fmt --all

clippy:
	cargo clippy --locked --package render-latex-worker --all-targets -- -D warnings

rust-test:
	cargo test --locked --package render-latex-worker

PYTHON ?= python3
smoke-worker: build-worker
	$(PYTHON) tests/worker_smoke.py target/release/render-latex-worker

smoke-base:
	nvim --headless -u repro/repro.lua repro/sample.md '+lua vim.defer_fn(function() require("render_latex").status(); vim.cmd("qa") end, 1000)'

smoke-long-note:
	nvim --headless -u repro/long_note.lua repro/long-note.md '+lua vim.defer_fn(function() require("render_latex").status(); vim.cmd("qa") end, 1000)'

smoke-light-theme:
	nvim --headless -u repro/light_theme.lua repro/sample.md '+lua vim.defer_fn(function() require("render_latex").status(); vim.cmd("qa") end, 1000)'

smoke-obsidian:
	nvim --headless -u repro/obsidian.lua repro/obsidian-vault/index.md '+lua vim.defer_fn(function() require("render_latex").status(); vim.cmd("qa") end, 1000)'

smoke-render-markdown:
	nvim --headless -u repro/render_markdown.lua repro/render-markdown-smoke.md '+lua vim.defer_fn(function() require("render_latex").status(); vim.cmd("qa") end, 1000)'

smoke-render-markdown-rich:
	nvim --headless -u repro/render_markdown.lua repro/render-markdown-sample.md '+lua vim.defer_fn(function() require("render_latex").status(); vim.cmd("qa") end, 1500)'

smoke-jupynvim:
	nvim --headless -u repro/jupynvim.lua repro/jupynvim-display-math.ipynb '+lua vim.defer_fn(function() require("render_latex").status(); vim.cmd("qa") end, 2000)'

smoke-doctor:
	nvim --headless -u repro/render_markdown.lua repro/render-markdown-smoke.md '+lua vim.defer_fn(function() require("render_latex").doctor(); vim.cmd("qa") end, 1000)'

smoke-health:
	nvim --headless -u repro/repro.lua repro/sample.md '+checkhealth render_latex' '+qa'

smoke-help:
	nvim --headless -u repro/repro.lua '+help render-latex' '+qa'

smoke-integrations: smoke-obsidian smoke-render-markdown

smoke-repros: smoke-base smoke-long-note smoke-light-theme smoke-obsidian smoke-render-markdown smoke-render-markdown-rich smoke-doctor smoke-health smoke-help

check: lint actionlint typecheck test clippy rust-test
