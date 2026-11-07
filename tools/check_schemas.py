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

schema = json.loads((root / "schema/islet.toml/1.json").read_text())
validator = Draft202012Validator(schema)
for path in sorted((root / "examples").glob("islet*.toml")):
    errors = list(validator.iter_errors(tomllib.loads(path.read_text())))
    for e in errors:
        print(f"{path.relative_to(root)}: {e.message}")
    failed |= bool(errors)
    print(f"{'FAIL' if errors else 'ok'} example {path.relative_to(root)}")

sys.exit(1 if failed else 0)
