.PHONY: gen

gen: src/generated.rs

src/generated.rs: gen.py $(wildcard data/*.txt)
	python3 gen.py > src/generated.rs
	cargo fmt
