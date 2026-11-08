"""Check every schema against the JSON Schema metaschema and every example file against its schema.

The examples are read with PyYAML, a full YAML reader, so this also proves the strict subset Islet writes means
the same thing to other YAML tools."""
import json
import pathlib
import sys
import yaml

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
        errors = list(validator.iter_errors(yaml.safe_load(path.read_text())))
        for e in errors:
            print(f"{path.relative_to(root)}: {e.message}")
        failed |= bool(errors)
        print(f"{'FAIL' if errors else 'ok'} example {path.relative_to(root)}")


validate("schema/islet/1.json", "islet*.yaml")
validate("schema/islet/1.json", "*/islet.yaml")
validate("schema/monorepo/1.json", "monorepo*.yaml")



def check_lock(path):
    """A full YAML reader must see the same types the strict reader does: versions and hashes are text."""
    global failed
    doc = yaml.safe_load(path.read_text())
    problems = []
    if doc.get("format") != 1:
        problems.append("format is not the number 1")
    for key, version in doc.get("catalog", {}).items():
        if not isinstance(version, str):
            problems.append(f"catalog {key} is {type(version).__name__}, not text")
    for member, deps in doc.get("members", {}).items():
        for key, dep in deps.items():
            version = dep.get("version") if isinstance(dep, dict) else dep
            if isinstance(dep, dict) and "path" in dep:
                continue
            if not isinstance(version, str):
                problems.append(f"{member} {key} version is {type(version).__name__}, not text")
    for key, pkg in doc.get("packages", {}).items():
        if not (isinstance(pkg["hash"], str) and len(pkg["hash"]) == 64):
            problems.append(f"{key} hash is not 64 characters of text")
        for need in pkg.get("needs", []):
            if need not in doc["packages"]:
                problems.append(f"{key} needs {need}, which is not a package")
    for p in problems:
        print(f"{path.relative_to(root)}: {p}")
    failed |= bool(problems)
    print(f"{'FAIL' if problems else 'ok'} example {path.relative_to(root)}")


for lock in sorted((root / "examples").glob("*.lock")):
    check_lock(lock)

sys.exit(1 if failed else 0)
