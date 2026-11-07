"""Check every schema against the JSON Schema metaschema and every example manifest against its schema."""
import json
import pathlib
import sys
import tomllib

from jsonschema import Draft202012Validator

root = pathlib.Path(__file__).resolve().parent.parent
failed = False

for path in sorted((root / "schema").rglob("*.json")):
    Draft202012Validator.check_schema(json.loads(path.read_text()))
    print(f"ok schema {path.relative_to(root)}")

def validate(schema_path, pattern):
    global failed
    validator = Draft202012Validator(json.loads((root / schema_path).read_text()))
    for path in sorted((root / "examples").glob(pattern)):
        errors = list(validator.iter_errors(tomllib.loads(path.read_text())))
        for e in errors:
            print(f"{path.relative_to(root)}: {e.message}")
        failed |= bool(errors)
        print(f"{'FAIL' if errors else 'ok'} example {path.relative_to(root)}")


validate("schema/islet.toml/1.json", "islet*.toml")
validate("schema/islet.toml/1.json", "*/islet.toml")
validate("schema/monorepo.toml/1.json", "monorepo*.toml")

sys.exit(1 if failed else 0)
