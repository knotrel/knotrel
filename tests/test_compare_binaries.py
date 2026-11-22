#!/usr/bin/env python3
"""Tests for scripts/compare-binaries.py on small synthetic Mach-O and ELF files."""

import contextlib
import hashlib
import importlib.util
import io
import struct
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("compare_binaries", ROOT / "scripts" / "compare-binaries.py")
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)


def adhoc_signature(signed: bytes, ident: bytes = b"knotrel_server") -> bytes:
    """Build an embedded-signature SuperBlob with one CodeDirectory whose
    SHA-256 code slots hash ``signed`` in 4 KiB pages (as the linker does)."""
    page = 4096
    n_code = (len(signed) + page - 1) // page
    ident = ident + b"\0"
    hash_off = 44 + len(ident)
    cd_len = hash_off + 32 * n_code
    cd = struct.pack(
        ">IIIIIIIIIBBBBI", 0xFADE0C02, cd_len, 0x20100, 2, hash_off, 44, 0, n_code,
        len(signed), 32, 2, 0, 12, 0,
    ) + ident
    for i in range(n_code):
        cd += hashlib.sha256(signed[i * page : (i + 1) * page]).digest()
    return struct.pack(">IIIII", 0xFADE0CC0, 20 + len(cd), 1, 0, 20) + cd


def macho(text: bytes, cstring: bytes, uuid: bytes, ident: bytes = b"knotrel_server",
          corrupt_signature: bool = False) -> bytes:
    """Build a minimal thin arm64 Mach-O: one __TEXT segment with __text and
    __cstring, an LC_UUID and a valid ad-hoc LC_CODE_SIGNATURE at the end."""
    seg_cmdsize = 72 + 2 * 80
    header_size = 32 + seg_cmdsize + 24 + 16
    text_off = header_size
    cstr_off = text_off + len(text)
    sig_off = cstr_off + len(cstring)
    sig_len = len(adhoc_signature(bytes(sig_off), ident))

    def section(name: bytes, offset: int, size: int) -> bytes:
        return struct.pack(
            "<16s16sQQIIIIIIII", name, b"__TEXT", 0, size, offset, 0, 0, 0, 0, 0, 0, 0
        )

    segment = struct.pack(
        "<II16sQQQQiiII", 0x19, seg_cmdsize, b"__TEXT", 0, 0, 0, 0, 5, 5, 2, 0
    ) + section(b"__text", text_off, len(text)) + section(b"__cstring", cstr_off, len(cstring))
    uuid_cmd = struct.pack("<II", 0x1B, 24) + uuid
    sig_cmd = struct.pack("<IIII", 0x1D, 16, sig_off, sig_len)
    header = struct.pack("<IiiIIIII", 0xFEEDFACF, 0x0100000C, 0, 2, 3, seg_cmdsize + 40, 0, 0)
    signed = header + segment + uuid_cmd + sig_cmd + text + cstring
    assert len(signed) == sig_off
    signature = bytearray(adhoc_signature(signed, ident))
    if corrupt_signature:
        signature[-1] ^= 0xFF
    return signed + bytes(signature)


def elf(text: bytes, rodata: bytes) -> bytes:
    """Build a minimal x86-64 ELF with .text, .rodata and .shstrtab sections."""
    shstrtab = b"\0.text\0.rodata\0.shstrtab\0"
    text_off = 64
    ro_off = text_off + len(text)
    str_off = ro_off + len(rodata)
    shoff = str_off + len(shstrtab)
    ident = b"\x7fELF" + bytes([2, 1, 1]) + bytes(9)
    header = ident + struct.pack(
        "<HHIQQQIHHHHHH", 2, 0x3E, 1, 0, 0, shoff, 0, 64, 0, 0, 64, 4, 3
    )

    def sh(name: int, sh_type: int, offset: int, size: int) -> bytes:
        return struct.pack("<IIQQQQIIQQ", name, sh_type, 0, 0, offset, size, 0, 0, 1, 0)

    sections = (
        sh(0, 0, 0, 0)
        + sh(1, 1, text_off, len(text))
        + sh(7, 1, ro_off, len(rodata))
        + sh(15, 3, str_off, len(shstrtab))
    )
    return header + text + rodata + shstrtab + sections


class CompareBinariesTest(unittest.TestCase):
    def run_compare(self, ref: bytes, new: bytes) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as tmp:
            a, b = Path(tmp, "a"), Path(tmp, "b")
            a.write_bytes(ref)
            b.write_bytes(new)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                status = compare.main(["compare-binaries.py", str(a), str(b)])
        return status, out.getvalue()

    def test_identical(self):
        data = macho(b"\x01" * 16, b"abc\0", b"U" * 16)
        self.assertEqual(self.run_compare(data, data)[0], 0)

    def test_macho_uuid_and_signature_only(self):
        ref = macho(b"\x01" * 16, b"abc\0", b"U" * 16)
        new = macho(b"\x01" * 16, b"abc\0", b"V" * 16)
        status, out = self.run_compare(ref, new)
        self.assertEqual(status, 2)
        self.assertIn("VERDICT byte-identical: no", out)
        self.assertIn("VERDICT equivalent after excluding metadata: yes; excluded: LC_UUID (16 bytes", out)

    def test_macho_string_difference_is_code_identical(self):
        ref = macho(b"\x01" * 16, b"13:14:50\0", b"U" * 16)
        new = macho(b"\x01" * 16, b"17:05:05\0", b"V" * 16)
        status, out = self.run_compare(ref, new)
        self.assertEqual(status, 4)
        self.assertRegex(out, r"__TEXT,__cstring\s+9\s+9\s+DIFFERENT")
        self.assertRegex(out, r"__TEXT,__text\s+16\s+16\s+identical")

    def test_macho_code_difference(self):
        ref = macho(b"\x01" * 16, b"abc\0", b"U" * 16)
        new = macho(b"\x02" * 16, b"abc\0", b"U" * 16)
        self.assertEqual(self.run_compare(ref, new)[0], 1)

    def test_macho_signature_must_follow_from_content(self):
        ref = macho(b"\x01" * 16, b"abc\0", b"U" * 16)
        # A signature that does not hash the file, or that differs in more than
        # the page hashes (identifier), is not excluded.
        bad = macho(b"\x01" * 16, b"abc\0", b"V" * 16, corrupt_signature=True)
        status, out = self.run_compare(ref, bad)
        self.assertNotEqual(status, 2)
        self.assertIn("page 0 hash does not match the file", out)
        other_ident = macho(b"\x01" * 16, b"abc\0", b"V" * 16, ident=b"knotrel_serveX")
        status, out = self.run_compare(ref, other_ident)
        self.assertNotEqual(status, 2)
        self.assertIn("signatures equal apart from page hashes: no", out)

    def test_elf_levels(self):
        ref = elf(b"\x90" * 8, b"Nov 21 2026\0")
        self.assertEqual(self.run_compare(ref, elf(b"\x90" * 8, b"Nov 22 2026\0"))[0], 4)
        self.assertEqual(self.run_compare(ref, elf(b"\xcc" * 8, b"Nov 21 2026\0"))[0], 1)
        sections = compare.elf_sections(ref)
        self.assertEqual(sections[".text"], b"\x90" * 8)

    def test_unsupported_format(self):
        self.assertEqual(self.run_compare(b"MZ" + bytes(62), b"MZ" + bytes(61) + b"\1")[0], 1)


if __name__ == "__main__":
    unittest.main()
