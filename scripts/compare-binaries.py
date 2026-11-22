#!/usr/bin/env python3
"""Compare two release executables section by section.

Used by ``scripts/check-reproducible-build.sh``. Prints the SHA-256 of both
files and, when they differ, a per-section table (sizes and equality) so that
a mismatch can be attributed to code (``__TEXT,__text`` / ``.text``),
embedded strings and paths (``__TEXT,__cstring``, ``.rodata``), unwind
information, or linker metadata (``LC_UUID``, code signature, build ID).

Supported formats: 64-bit little-endian ELF and thin 64-bit Mach-O. Other
formats (PE) are compared by whole-file hash only.

It reports two separate verdicts and never merges them:

- **byte-identical**: the two files have the same SHA-256.
- **equivalent after excluding named metadata** (Mach-O only): the files are
  equal once exactly these byte ranges are zeroed in both, and nothing else:

  - the 16-byte ``LC_UUID`` payload. ``ld`` computes it as a hash over the
    output *including* the debug map (``N_OSO`` entries naming input objects
    by absolute path, e.g. the Rust sysroot's ``libcompiler_builtins`` rlib);
    ``rustc`` strips the debug map after linking (release default
    ``strip = "debuginfo"``), so the path changes the UUID but appears nowhere
    in the binary;
  - the ``LC_CODE_SIGNATURE`` blob: the linker's ad-hoc signature, a list of
    SHA-256 hashes of the file's pages, one of which holds the UUID.

  The excluded ranges are printed. For ELF the verdict is "not applicable":
  nothing is excluded (``.note.gnu.build-id`` is derived from the content and
  is compared like any other section).

A third, diagnostic line says whether the code section is identical: only the
bytes of ``__TEXT,__text`` (Mach-O) or ``.text`` (ELF) are compared; data,
strings, unwind tables, symbols, fixups and load commands are not. This level
alone cannot close the reproducible-build gate.

Exit status (strongest result reached):

  0  byte-identical
  2  not byte-identical; equivalent after excluding ``LC_UUID`` and
     ``LC_CODE_SIGNATURE`` (Mach-O)
  4  not byte-identical, not equivalent; code section identical
  1  code section differs, or unsupported format and files differ
  3  usage or parse error

Only the standard library is used; run it with ``python3 -I``. Time and space
are linear in the file sizes.
"""

from __future__ import annotations

import hashlib
import struct
import sys
from pathlib import Path

CODE_SECTIONS = {"__TEXT,__text", ".text"}

# Mach-O load commands that are `linkedit_data_command` (dataoff, datasize).
LINKEDIT_DATA_COMMANDS = {
    0x1E: "LC_SEGMENT_SPLIT_INFO",
    0x26: "LC_FUNCTION_STARTS",
    0x29: "LC_DATA_IN_CODE",
    0x80000033: "LC_DYLD_EXPORTS_TRIE",
    0x80000034: "LC_DYLD_CHAINED_FIXUPS",
}
LC_SEGMENT_64 = 0x19
LC_SYMTAB = 0x2
LC_UUID = 0x1B
LC_CODE_SIGNATURE = 0x1D
LC_BUILD_VERSION = 0x32


def sha256(data: bytes) -> str:
    """Return the hexadecimal SHA-256 digest of ``data``."""
    return hashlib.sha256(data).hexdigest()


def elf_sections(data: bytes) -> dict[str, bytes]:
    """Return ``{name: contents}`` for every section of a 64-bit LE ELF file.

    ``SHT_NOBITS`` sections (``.bss``) map to an empty byte string.
    """
    if data[4] != 2 or data[5] != 1:
        raise ValueError("only 64-bit little-endian ELF is supported")
    e_shoff = struct.unpack_from("<Q", data, 0x28)[0]
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 0x3A)
    headers = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        name, sh_type, _flags, _addr, sh_offset, sh_size = struct.unpack_from(
            "<IIQQQQ", data, off
        )
        headers.append((name, sh_type, sh_offset, sh_size))
    _, _, str_off, str_size = headers[e_shstrndx]
    strtab = data[str_off : str_off + str_size]
    sections: dict[str, bytes] = {}
    for name_off, sh_type, sh_offset, sh_size in headers:
        end = strtab.index(b"\0", name_off)
        name = strtab[name_off:end].decode("ascii", "replace") or "<null>"
        body = b"" if sh_type == 8 else data[sh_offset : sh_offset + sh_size]
        sections[name] = body
    return sections


def macho_load_commands(data: bytes):
    """Yield ``(cmd, offset, cmdsize)`` for each load command of a thin Mach-O."""
    magic, _cpu, _sub, _ftype, ncmds = struct.unpack_from("<IiiII", data, 0)
    if magic != 0xFEEDFACF:
        raise ValueError("only thin 64-bit little-endian Mach-O is supported")
    off = 32
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", data, off)
        yield cmd, off, cmdsize
        off += cmdsize


def macho_sections(data: bytes) -> dict[str, bytes]:
    """Return ``{"SEG,sect": contents}`` plus selected load commands.

    Besides every section of every ``LC_SEGMENT_64``, the result contains
    ``LC_UUID``, ``LC_BUILD_VERSION`` (platform, SDK and linker version), the
    symbol and string tables, other ``__LINKEDIT`` data (fixups, exports,
    function starts) and the code signature blob, so that linker metadata
    differences are visible. Zero-fill sections map to an empty byte string.
    """
    sections: dict[str, bytes] = {}
    for cmd, off, cmdsize in macho_load_commands(data):
        if cmd == LC_SEGMENT_64:
            nsects = struct.unpack_from("<I", data, off + 64)[0]
            sect_off = off + 72
            for _ in range(nsects):
                sectname = data[sect_off : sect_off + 16].rstrip(b"\0").decode()
                segname = data[sect_off + 16 : sect_off + 32].rstrip(b"\0").decode()
                _addr, size, offset = struct.unpack_from("<QQI", data, sect_off + 32)
                flags = struct.unpack_from("<I", data, sect_off + 64)[0]
                zerofill = (flags & 0xFF) in (0x1, 0xC, 0x12)
                body = b"" if zerofill else data[offset : offset + size]
                sections[f"{segname},{sectname}"] = body
                sect_off += 80
        elif cmd == LC_SYMTAB:
            symoff, nsyms, stroff, strsize = struct.unpack_from("<IIII", data, off + 8)
            sections["<LC_SYMTAB symbols>"] = data[symoff : symoff + nsyms * 16]
            sections["<LC_SYMTAB strings>"] = data[stroff : stroff + strsize]
        elif cmd in LINKEDIT_DATA_COMMANDS:
            dataoff, datasize = struct.unpack_from("<II", data, off + 8)
            name = LINKEDIT_DATA_COMMANDS[cmd]
            sections[f"<{name}>"] = data[dataoff : dataoff + datasize]
        elif cmd == LC_UUID:
            sections["<LC_UUID>"] = data[off + 8 : off + 24]
        elif cmd == LC_BUILD_VERSION:
            sections["<LC_BUILD_VERSION>"] = data[off : off + cmdsize]
        elif cmd == LC_CODE_SIGNATURE:
            dataoff, datasize = struct.unpack_from("<II", data, off + 8)
            sections["<LC_CODE_SIGNATURE>"] = data[dataoff : dataoff + datasize]
    return sections


def macho_excluded_ranges(data: bytes) -> list[tuple[str, int, int]]:
    """Return ``(name, offset, length)`` of the metadata excluded from the
    equivalence verdict: the ``LC_UUID`` payload and the code signature blob."""
    ranges = []
    for cmd, off, _cmdsize in macho_load_commands(data):
        if cmd == LC_UUID:
            ranges.append(("LC_UUID", off + 8, 16))
        elif cmd == LC_CODE_SIGNATURE:
            dataoff, datasize = struct.unpack_from("<II", data, off + 8)
            ranges.append(("LC_CODE_SIGNATURE", dataoff, datasize))
    return ranges


def signature_check(data: bytes) -> tuple[bool, bytes, str]:
    """Check the ad-hoc signature of a Mach-O against the file it signs.

    Parses the ``LC_CODE_SIGNATURE`` SuperBlob, finds the CodeDirectory
    (magic ``0xfade0c02``), recomputes the SHA-256 of every code page up to
    ``codeLimit`` and compares it with the stored code-slot hash. Returns
    ``(valid, blob_without_code_slots, reason)``, where the second item is the
    signature blob with the code-slot hash array zeroed. Two binaries whose
    signatures are both valid and whose remaining blobs are equal differ in
    the signature only through the page hashes, i.e. only as a consequence of
    differences in the signed pages (such as the ``LC_UUID``).
    """
    sig = next(
        ((o, ln) for n, o, ln in macho_excluded_ranges(data) if n == "LC_CODE_SIGNATURE"),
        None,
    )
    if sig is None:
        return False, b"", "no LC_CODE_SIGNATURE"
    base, length = sig
    blob = bytearray(data[base : base + length])
    magic, _len, count = struct.unpack_from(">III", blob, 0)
    if magic != 0xFADE0CC0:
        return False, bytes(blob), "not an embedded signature SuperBlob"
    for i in range(count):
        _slot, off = struct.unpack_from(">II", blob, 12 + 8 * i)
        if struct.unpack_from(">I", blob, off)[0] != 0xFADE0C02:
            continue
        hash_offset, ident_offset, n_special, n_code, code_limit = struct.unpack_from(
            ">IIIII", blob, off + 16
        )
        hash_size, hash_type, _platform, page_log2 = struct.unpack_from(">BBBB", blob, off + 36)
        if hash_type != 2 or hash_size != 32:
            return False, bytes(blob), f"unsupported hash type {hash_type}"
        page = 1 << page_log2
        start = off + hash_offset
        for slot in range(n_code):
            chunk = data[slot * page : min((slot + 1) * page, code_limit)]
            stored = bytes(blob[start + 32 * slot : start + 32 * (slot + 1)])
            if hashlib.sha256(chunk).digest() != stored:
                return False, bytes(blob), f"page {slot} hash does not match the file"
        blob[start : start + 32 * n_code] = bytes(32 * n_code)
        return True, bytes(blob), f"{n_code} page hashes of {page} bytes verified"
    return False, bytes(blob), "no CodeDirectory"


def macho_masked(data: bytes) -> bytes:
    """Return ``data`` with the ranges of :func:`macho_excluded_ranges` zeroed."""
    out = bytearray(data)
    for _name, offset, length in macho_excluded_ranges(data):
        out[offset : offset + length] = bytes(length)
    return bytes(out)


def is_macho(data: bytes) -> bool:
    """Return whether ``data`` starts with the thin 64-bit Mach-O magic."""
    return data[:4] == b"\xcf\xfa\xed\xfe"


def sections_of(data: bytes) -> dict[str, bytes] | None:
    """Dispatch on the file magic; return ``None`` for unsupported formats."""
    if data[:4] == b"\x7fELF":
        return elf_sections(data)
    if is_macho(data):
        return macho_sections(data)
    return None


def main(argv: list[str]) -> int:
    """Compare ``argv[1]`` (reference) with ``argv[2]`` (rebuild)."""
    if len(argv) != 3:
        print(f"usage: {argv[0]} <reference-binary> <rebuilt-binary>", file=sys.stderr)
        return 3
    ref_path, new_path = Path(argv[1]), Path(argv[2])
    ref, new = ref_path.read_bytes(), new_path.read_bytes()
    print(f"reference: {sha256(ref)}  {len(ref)} bytes  {ref_path}")
    print(f"rebuild:   {sha256(new)}  {len(new)} bytes  {new_path}")
    if ref == new:
        print("VERDICT byte-identical: yes")
        print("RESULT: byte-identical")
        return 0
    print("VERDICT byte-identical: no")

    try:
        ref_s, new_s = sections_of(ref), sections_of(new)
    except (ValueError, struct.error, IndexError, UnicodeDecodeError) as err:
        print(f"ERROR: cannot parse binaries: {err}", file=sys.stderr)
        return 3
    if ref_s is None or new_s is None:
        print("VERDICT equivalent after excluding metadata: not applicable (unsupported format)")
        print("RESULT: different")
        return 1

    print(f"\n{'section':<32} {'ref size':>10} {'new size':>10}  status")
    code_identical = True
    code_seen = False
    for name in sorted(set(ref_s) | set(new_s)):
        a, b = ref_s.get(name), new_s.get(name)
        if a is None or b is None:
            status = "only in " + ("rebuild" if a is None else "reference")
        else:
            status = "identical" if a == b else "DIFFERENT"
        if name in CODE_SECTIONS:
            code_seen = True
            code_identical = code_identical and status == "identical"
        sa = "-" if a is None else str(len(a))
        sb = "-" if b is None else str(len(b))
        print(f"{name:<32} {sa:>10} {sb:>10}  {status}")
    print()

    equivalent = False
    if is_macho(ref) and is_macho(new):
        ref_r, new_r = macho_excluded_ranges(ref), macho_excluded_ranges(new)
        equivalent = ref_r == new_r and macho_masked(ref) == macho_masked(new)
        listed = ", ".join(f"{n} ({ln} bytes at offset {o})" for n, o, ln in ref_r)
        differing = sum(1 for x, y in zip(ref, new) if x != y) if len(ref) == len(new) else None
        ref_ok, ref_rest, ref_why = signature_check(ref)
        new_ok, new_rest, new_why = signature_check(new)
        sig_follows = ref_ok and new_ok and ref_rest == new_rest
        equivalent = equivalent and sig_follows
        print(
            "VERDICT equivalent after excluding metadata: "
            + ("yes" if equivalent else "no")
            + f"; excluded: {listed}"
            + ("" if differing is None else f"; {differing} differing bytes in total")
        )
        print(
            "NOTE LC_CODE_SIGNATURE is the linker's ad-hoc signature: a SHA-256 hash per page, "
            "and the first page holds LC_UUID, so a different UUID changes it. "
            f"Reference: {ref_why}; rebuild: {new_why}; signatures equal apart from page hashes: "
            + ("yes" if ref_rest == new_rest else "no")
        )
    else:
        print("VERDICT equivalent after excluding metadata: not applicable (ELF: nothing is excluded)")
    code_ok = code_seen and code_identical
    print("VERDICT code section identical: " + ("yes" if code_ok else "no"))

    if equivalent:
        print("RESULT: equivalent-excluding-metadata (not byte-identical)")
        return 2
    if code_ok:
        print("RESULT: code-identical (not byte-identical, not equivalent)")
        return 4
    print("RESULT: different")
    return 1

if __name__ == "__main__":
    sys.exit(main(sys.argv))
