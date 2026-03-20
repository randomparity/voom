.PHONY: build clean test wit

WIT_SRC := ../../crates/voom-wit/wit
WIT_DST := wit

# Symlink WIT files from the shared crate
wit: $(WIT_DST)/types.wit $(WIT_DST)/host.wit $(WIT_DST)/plugin.wit $(WIT_DST)/world.wit

$(WIT_DST)/%.wit: $(WIT_SRC)/%.wit
	@mkdir -p $(WIT_DST)
	ln -sf $(abspath $<) $@

build: wit
	componentize-py -d $(WIT_DST) -w voom-plugin componentize -o tvdb-metadata.wasm src/tvdb_metadata/plugin

test:
	python -m pytest tests/ -v

clean:
	rm -rf $(WIT_DST) tvdb-metadata.wasm __pycache__ .pytest_cache
	find . -type d -name __pycache__ -exec rm -rf {} + 2>/dev/null || true
