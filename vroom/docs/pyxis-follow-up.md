# Pyxis Follow-Up Pack

This pack includes the three immediate follow-up deliverables for the two-SDK system:

1. `schemas/pyxis.schema.json` — JSON Schema for `pyxis.json`
2. `schemas/appinfo.schema.json` — JSON Schema for `appinfo.json`
3. `tools/pyxis-package.sh` — reference packaging script

## Design notes

- Schemas target **JSON Schema Draft 2020-12**.
- `pyxis.json` is the developer project file.
- `appinfo.json` is the installed app manifest.
- Web apps require package architecture `all`.
- Native apps must not use architecture `all`.
- Web `main` must be relative.
- Native `main` must be absolute.

## Reference packaging script

`pyxis-package.sh` is intentionally a reference implementation, not a final production packager.

It currently:
- reads `pyxis.json`
- stages app files under the Pyxis filesystem layout
- generates `appinfo.json`
- generates `CONTROL/control`
- generates a default `postinst`
- emits an `.ipk` into `dist/`

## Important implementation note

The script creates an IPK-like artifact for prototyping and contract validation. In production, you should switch the final packaging step to `opkg-build` or a known-good IPK builder compatible with your target environment.

## Suggested next tasks

1. Add schema validation to the CLI with Ajv or another Draft 2020-12 validator.
2. Replace the script's final archive step with `opkg-build`.
3. Add `pyxis install` over SSH.
4. Add templating commands: `pyxis-sdk new` and `pyxis-pdk new`.
5. Generate typed config bindings for TypeScript, Rust, or Go from the schemas.
