#!/usr/bin/env python3
"""Report breaking changes between two versions of docs/openapi.yaml.

Implements the HTTP rules of docs/compatibility.md, section 3, for stable
operations. Stability is taken from BASE: changes to an operation marked
`x-knotrel-stability: experimental` there are reported but never fail the
check, and marking a stable operation experimental is itself a breaking
change (otherwise the marker would silence every other break).

Usage:
  check-openapi-compat.py BASE.yaml HEAD.yaml [--approved FILE]

Exit status: 0 when every breaking change is approved (or there is none),
1 otherwise, 2 for usage or input errors. FILE is a YAML list of entries with
`change` (the exact message printed below), `release` and `justification`.
A change to a value of a named schema (enum value, enum, constant) can also be
approved once by its schema-level message, printed on failure as
`schema-level change ...: <$ref>[/properties/<name>...]: <change>`; it covers
every operation that reaches that schema.

Rules, for each operation present in BASE:
- the operation, a parameter, a documented response status or a documented
  response header disappears;
- a parameter, the request body or a request-body property becomes required;
- a request-body property, accepted enum value or `oneOf`/`anyOf`
  alternative disappears, a request type narrows, or a request constraint
  tightens (`maxLength`, `minLength`, `maximum`, `minimum`, `maxItems`,
  `minItems`, `pattern`);
- a response property disappears or stops being required, a documented
  response enum value disappears (an error code, an `engine` value, a status
  or mode), a response enum is dropped (the set of values becomes open), a
  response constant changes or disappears, or a response type widens (for example a field becomes nullable, also through a `null`
  alternative of `oneOf`/`anyOf`).
`oneOf`/`anyOf` alternatives present in both versions (matched by reference,
otherwise by position) are compared recursively.
Additions (operations, optional request fields, response fields, response
enum values) are compatible.
"""

import sys

import yaml

METHODS = ("get", "put", "post", "delete", "patch", "head", "options")


class Spec:
    def __init__(self, doc):
        self.doc = doc

    def resolve(self, schema):
        """Follows `$ref`s; returns (schema, ref name or None)."""
        name = None
        seen = 0
        while isinstance(schema, dict) and "$ref" in schema:
            ref = schema["$ref"]
            if not ref.startswith("#/"):
                raise ValueError(f"unsupported reference {ref}")
            name = ref
            node = self.doc
            for part in ref[2:].split("/"):
                node = node[part]
            schema = node
            seen += 1
            if seen > 50:
                raise ValueError(f"reference cycle at {ref}")
        return schema if isinstance(schema, dict) else {}, name

    def flatten(self, schema):
        """Merges `allOf` members into one object view."""
        schema, _ = self.resolve(schema)
        if "allOf" not in schema:
            return schema
        merged = {k: v for k, v in schema.items() if k != "allOf"}
        props = dict(merged.get("properties", {}))
        required = list(merged.get("required", []))
        for member in schema["allOf"]:
            part = self.flatten(member)
            props.update(part.get("properties", {}))
            required += part.get("required", [])
            for key in ("type", "enum", "const", "items", "additionalProperties"):
                if key in part and key not in merged:
                    merged[key] = part[key]
        if props:
            merged["properties"] = props
        if required:
            merged["required"] = sorted(set(required))
        return merged


def types(schema):
    t = schema.get("type")
    if t is None:
        return None
    return set(t) if isinstance(t, list) else {t}


def nullable(spec, schema, depth=0):
    if schema.get("nullable") is True or "null" in (types(schema) or set()):
        return True
    if depth > 20:
        return False
    return any(nullable(spec, spec.flatten(m), depth + 1)
               for comb in ("oneOf", "anyOf") for m in schema.get(comb, []))


# Request constraints that reject more values when they increase (`min`) or
# decrease (`max`).
LOWER_BOUNDS = ("minLength", "minimum", "exclusiveMinimum", "minItems", "minProperties")
UPPER_BOUNDS = ("maxLength", "maximum", "exclusiveMaximum", "maxItems", "maxProperties")


def tightened(b, h):
    out = []
    for key in LOWER_BOUNDS:
        if isinstance(h.get(key), (int, float)) and not isinstance(h.get(key), bool):
            if key not in b or h[key] > b[key]:
                out.append(f"{key} {b.get(key)} -> {h[key]}")
    for key in UPPER_BOUNDS:
        if isinstance(h.get(key), (int, float)) and not isinstance(h.get(key), bool):
            if key not in b or h[key] < b[key]:
                out.append(f"{key} {b.get(key)} -> {h[key]}")
    if "pattern" in h and h.get("pattern") != b.get("pattern"):
        out.append("pattern changed")
    return out


def matched_members(base_spec, head_spec, bmembers, hmembers):
    """Pairs alternatives by reference name, inline ones by position."""
    href = {head_spec.resolve(m)[1]: m for m in hmembers if head_spec.resolve(m)[1]}
    pairs = []
    for i, bm in enumerate(bmembers):
        name = base_spec.resolve(bm)[1]
        if name:
            if name in href:
                pairs.append((name, bm, href[name]))
        elif i < len(hmembers) and not head_spec.resolve(hmembers[i])[1]:
            pairs.append((str(i), bm, hmembers[i]))
    return pairs


def variants(spec, schema, key):
    return {spec.resolve(v)[1] or repr(v) for v in schema.get(key, [])}


class Finding(str):
    """A message that a schema-level approval can also cover.

    `schema_change` names the same change by its location in BASE (the
    `$ref` of the schema that declares the value, followed by the property
    path, `/items` or the `oneOf`/`anyOf` member), so one approval entry
    covers every operation that reaches a shared schema.
    """

    def __new__(cls, message, schema_change=None):
        obj = super().__new__(cls, message)
        obj.schema_change = schema_change
        return obj


def compare(base_spec, head_spec, base, head, direction, where, out, seen, loc=None):
    """Appends breaking changes of one schema pair to `out`."""
    _, bref = base_spec.resolve(base)
    if bref:
        loc = bref
    _, href = head_spec.resolve(head)
    key = (bref, href, direction)
    if bref and href:
        if key in seen:
            return
        seen.add(key)
    b = base_spec.flatten(base)
    h = head_spec.flatten(head)

    bt, ht = types(b), types(h)
    if bt is not None and ht is not None:
        if direction == "request" and not bt <= ht:
            out.append(f"{where}: request type narrowed from {sorted(bt)} to {sorted(ht)}")
        if direction == "response" and not ht <= bt:
            out.append(f"{where}: response type widened from {sorted(bt)} to {sorted(ht)}")
    if direction == "response" and nullable(head_spec, h) and not nullable(base_spec, b):
        out.append(f"{where}: response value became nullable")
    if direction == "request":
        for change in tightened(b, h):
            out.append(f"{where}: request constraint tightened ({change})")

    def schema_finding(change):
        return Finding(f"{where}: {change}", f"{loc}: {change}" if loc else None)

    if "enum" in b and "enum" in h:
        removed = [v for v in b["enum"] if v not in h["enum"]]
        if removed and direction == "request":
            out.append(f"{where}: accepted values removed {removed}")
        elif removed:
            out.append(schema_finding(f"response values removed {removed}"))
    elif "enum" in b and direction == "response":
        # A closed set of documented values becomes open, unless HEAD pins
        # one of them with `const`.
        if "const" not in h or h["const"] not in b["enum"]:
            out.append(schema_finding("response enum removed"))
    if "const" in b and direction == "request" and h.get("const", b["const"]) != b["const"]:
        out.append(f"{where}: accepted constant changed")
    if "const" in b and direction == "response":
        if "const" in h:
            if h["const"] != b["const"]:
                out.append(schema_finding(f"response constant changed from {b['const']!r} to {h['const']!r}"))
        elif h.get("enum") != [b["const"]]:
            out.append(schema_finding(f"response constant removed ({b['const']!r})"))

    for comb in ("oneOf", "anyOf"):
        if comb in b and comb in h and direction == "request":
            gone = variants(base_spec, b, comb) - variants(head_spec, h, comb)
            if gone:
                out.append(f"{where}: request alternatives removed {sorted(gone)}")
        if comb in b and comb in h:
            for label, bm, hm in matched_members(base_spec, head_spec, b[comb], h[comb]):
                compare(base_spec, head_spec, bm, hm, direction, f"{where}<{comb} {label}>", out, seen,
                        f"{loc}/{comb}/{label}" if loc else None)

    bprops = b.get("properties", {})
    hprops = h.get("properties", {})
    breq = set(b.get("required", []))
    hreq = set(h.get("required", []))
    for name, bprop in bprops.items():
        path = f"{where}.{name}"
        if name not in hprops:
            if direction == "response":
                out.append(f"{path}: response property removed")
            else:
                out.append(f"{path}: request property removed")
            continue
        if direction == "response" and name in breq and name not in hreq:
            out.append(f"{path}: response property no longer required")
        compare(base_spec, head_spec, bprop, hprops[name], direction, path, out, seen,
                f"{loc}/properties/{name}" if loc else None)
    if direction == "request":
        for name in sorted(hreq - breq):
            out.append(f"{where}.{name}: request property became required")

    if "items" in b and "items" in h:
        compare(base_spec, head_spec, b["items"], h["items"], direction, f"{where}[]", out, seen,
                f"{loc}/items" if loc else None)


def json_schema(spec, holder):
    holder, _ = spec.resolve(holder)
    content = holder.get("content", {})
    for media in ("application/json", "application/problem+json"):
        if media in content and "schema" in content[media]:
            return content[media]["schema"]
    return None


def experimental(op):
    return op.get("x-knotrel-stability") == "experimental"


def compare_headers(base, head, bresp, hresp, where, out):
    bheaders = base.resolve(bresp)[0].get("headers") or {}
    hheaders = head.resolve(hresp)[0].get("headers") or {}
    hlower = {k.lower(): v for k, v in hheaders.items()}
    for name, bh in bheaders.items():
        hh = hlower.get(name.lower())
        if hh is None:
            out.append(f"{where}: response header {name} removed")
            continue
        bh, hh = base.resolve(bh)[0], head.resolve(hh)[0]
        if "schema" in bh and "schema" in hh:
            compare(base, head, bh["schema"], hh["schema"], "response",
                    f"{where} header {name}", out, set())


def check(base_doc, head_doc):
    """Returns (breaking, informational) messages."""
    base, head = Spec(base_doc), Spec(head_doc)
    breaking, info = [], []
    for path, bitem in (base_doc.get("paths") or {}).items():
        hitem = (head_doc.get("paths") or {}).get(path, {})
        for method in METHODS:
            if method not in bitem:
                continue
            op_name = f"{method.upper()} {path}"
            bop = bitem[method]
            hop = hitem.get(method)
            found = []
            if hop is None:
                found.append(f"{op_name}: operation removed")
            else:
                bparams = {
                    (p["in"], p["name"]): p
                    for p in (base.resolve(x)[0] for x in bitem.get("parameters", []) + bop.get("parameters", []))
                }
                hparams = {
                    (p["in"], p["name"]): p
                    for p in (head.resolve(x)[0] for x in hitem.get("parameters", []) + hop.get("parameters", []))
                }
                for k, bp in bparams.items():
                    if k not in hparams:
                        found.append(f"{op_name}: parameter {k[1]} ({k[0]}) removed")
                    elif hparams[k].get("required") and not bp.get("required"):
                        found.append(f"{op_name}: parameter {k[1]} ({k[0]}) became required")
                    elif "schema" in bp and "schema" in hparams[k]:
                        compare(base, head, bp["schema"], hparams[k]["schema"], "request",
                                f"{op_name} parameter {k[1]}", found, set())
                for k, hp in hparams.items():
                    if k not in bparams and hp.get("required"):
                        found.append(f"{op_name}: new required parameter {k[1]} ({k[0]})")
                if "requestBody" in bop:
                    bbody = json_schema(base, bop["requestBody"])
                    hbody = json_schema(head, hop.get("requestBody", {})) if "requestBody" in hop else None
                    if bbody is not None and hbody is None:
                        found.append(f"{op_name}: request body schema removed")
                    elif bbody is not None:
                        compare(base, head, bbody, hbody, "request", f"{op_name} request", found, set())
                    if ("requestBody" in hop
                            and head.resolve(hop["requestBody"])[0].get("required")
                            and not base.resolve(bop["requestBody"])[0].get("required")):
                        found.append(f"{op_name}: request body became required")
                elif "requestBody" in hop and head.resolve(hop["requestBody"])[0].get("required"):
                    found.append(f"{op_name}: request body became required")
                for status, bresp in (bop.get("responses") or {}).items():
                    hresp = (hop.get("responses") or {}).get(status)
                    if hresp is None:
                        found.append(f"{op_name}: response {status} removed")
                        continue
                    compare_headers(base, head, bresp, hresp, f"{op_name} {status}", found)
                    bs = json_schema(base, bresp)
                    hs = json_schema(head, hresp)
                    if bs is not None and hs is None:
                        found.append(f"{op_name}: response {status} schema removed")
                    elif bs is not None:
                        compare(base, head, bs, hs, "response", f"{op_name} {status}", found, set())
            if experimental(bop):
                info += [f"(experimental) {m}" for m in found]
            else:
                if hop is not None and experimental(hop):
                    found.append(f"{op_name}: stable operation marked experimental")
                breaking += found
    return breaking, info


def main(argv):
    args = argv[1:]
    approved_path = None
    if "--approved" in args:
        i = args.index("--approved")
        if i + 1 >= len(args):
            print(__doc__, file=sys.stderr)
            return 2
        approved_path = args[i + 1]
        del args[i : i + 2]
    if len(args) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        with open(args[0], encoding="utf-8") as f:
            base_doc = yaml.safe_load(f)
        with open(args[1], encoding="utf-8") as f:
            head_doc = yaml.safe_load(f)
        approved = []
        if approved_path:
            with open(approved_path, encoding="utf-8") as f:
                approved = yaml.safe_load(f) or []
    except (OSError, yaml.YAMLError) as e:
        print(f"cannot read input: {e}", file=sys.stderr)
        return 2
    approved_changes = {entry["change"] for entry in approved if isinstance(entry, dict) and "change" in entry}
    breaking, info = check(base_doc, head_doc)
    for line in info:
        print(f"note: {line}")

    def is_approved(line):
        return line in approved_changes or getattr(line, "schema_change", None) in approved_changes

    unapproved = [b for b in breaking if not is_approved(b)]
    for line in breaking:
        status = "approved" if is_approved(line) else "BREAKING"
        print(f"{status}: {line}")
    if unapproved:
        shared = {}
        for line in unapproved:
            if getattr(line, "schema_change", None):
                shared[line.schema_change] = shared.get(line.schema_change, 0) + 1
        for change, count in shared.items():
            print(f"schema-level change (one approval entry covers its {count} message(s)): {change}")
        print(f"{len(unapproved)} unapproved breaking change(s) to stable operations "
              "(docs/compatibility.md, sections 3 and 6)")
        return 1
    print(f"OpenAPI compatible: {len(breaking)} approved breaking change(s), {len(info)} note(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
