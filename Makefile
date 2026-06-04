.PHONY: gen

gen: src/generated.rs
	python3 gen_date_tests.py > src/date_test_cases.txt

src/generated.rs: gen.py $(wildcard data/*.txt)
	python3 gen.py > src/generated.rs
	cargo fmt
