"""Tests for the large-scale mode's build and its C interface (rust/halo-client,
port/linux/game/large_mode.c, tools/rust_client.py).

The mode itself is tested where it runs: the library's own tests
(rust/halo-client/tests/boundary.rs, run by the workflow's rust-match job) and
the headless game against a local server (rust/halo-client/tests/headless.rs,
which needs the game's own data). What is here needs neither.
"""

import io
import re
from pathlib import Path
from types import SimpleNamespace

import pytest

from tools import linux_build, ninja_syntax, rust_client

ROOT = Path(__file__).resolve().parent.parent
FFI = ROOT / "rust/halo-client/src/ffi.rs"
ADAPTER = ROOT / "port/linux/game/large_mode.c"


# ---------- the boundary: only floats, 32-bit integers and pointers; nothing returned by value


# what a Rust type of the boundary is in the C of the 32-bit client
# (`unsigned long` is 32 bits there, as the adapter asserts)
RUST_TO_C = {"f32": "float", "u32": "unsigned long", "i32": "long", "c_char": "char"}


def rust_c_type(rust: str) -> str:
    """the C spelling of a Rust boundary type, without spaces; a type that may
    not cross the boundary fails the test"""
    rust = rust.strip()
    pointer = re.fullmatch(r"\*(const|mut) (\w+)", rust)
    if pointer:
        assert pointer.group(2) in RUST_TO_C, f"a pointer to {pointer.group(2)} crosses the boundary"
        const = "const" if pointer.group(1) == "const" else ""
        return f"{const}{RUST_TO_C[pointer.group(2)]}*".replace(" ", "")
    assert rust in ("f32", "u32", "i32"), f"{rust} crosses the boundary by value"
    return RUST_TO_C[rust].replace(" ", "")


def library_functions():
    """name -> (parameter types, return type) of the `extern "C"` functions of
    the library, in C spelling without spaces; the return type is "void" for none"""
    text = re.sub(r"//[^\n]*", "", FFI.read_text(encoding="utf-8"))
    functions = {}
    for match in re.finditer(r'pub (?:unsafe )?extern "C" fn (\w+)\(([^)]*)\)(?: -> ([^{]+))?\s*\{', text):
        name, parameters, result = match.groups()
        types = [rust_c_type(p.split(":", 1)[1]) for p in parameters.replace("\n", " ").split(",") if p.strip()]
        functions[name] = (types, rust_c_type(result) if result else "void")
    return functions


def adapter_prototypes():
    """the same, from the declarations in the adapter"""
    text = re.sub(r"/\*.*?\*/", "", ADAPTER.read_text(encoding="utf-8"), flags=re.S)
    functions = {}
    for match in re.finditer(r"^([a-z ]+?)\s+\*?(halo_large_\w+)\(([^;{]*)\);", text, flags=re.M | re.S):
        result, name, parameters = match.groups()
        types = []
        for parameter in parameters.replace("\n", " ").split(","):
            parameter = parameter.strip()
            if parameter and parameter != "void":
                # drop the parameter's name
                types.append(re.sub(r"\s", "", re.sub(r"\w+$", "", parameter)))
        functions[name] = (types, result.replace(" ", ""))
    return functions


def test_the_library_passes_only_floats_32_bit_integers_and_pointers():
    functions = library_functions()
    assert {"halo_large_start", "halo_large_stop", "halo_large_status", "halo_large_frame", "halo_large_unit",
            "halo_large_local", "halo_large_bounds", "halo_large_send_input", "halo_large_error",
            "halo_large_identity_dir", "halo_large_browse_start", "halo_large_browse_stop",
            "halo_large_browse_status", "halo_large_browse_list", "halo_large_browse_entry",
            "halo_large_browse_text", "halo_large_browse_find", "halo_large_browse_message",
            "halo_large_identity", "halo_large_refusal"} <= set(functions)
    for name, (parameters, result) in functions.items():
        # (rust_c_type refused anything else already) nothing comes back but a 32-bit integer
        assert result in ("void", "unsignedlong"), f"{name} returns {result}"
        assert parameters is not None


def test_the_adapter_declares_what_the_library_exports():
    library = library_functions()
    declared = adapter_prototypes()
    assert set(declared) == set(library), "the adapter and the library name different functions"
    for name, signature in library.items():
        assert declared[name] == signature, f"{name}: the adapter has {declared[name]}, the library {signature}"


def test_the_adapter_uses_the_library_only_when_the_build_has_it():
    text = ADAPTER.read_text(encoding="utf-8")
    # the declarations and every call are inside #ifdef HALO_LARGE_MODE, whose
    # #else is the build without the library: nothing of it is called there
    inside, _, outside = text.partition("#else\n\n/* no library in this build")
    assert "#ifdef HALO_LARGE_MODE" in inside
    assert "halo_large_" not in outside


# ---------- the build


def build_graph(large_mode: bool) -> str:
    out = io.StringIO()
    sln = SimpleNamespace(build_dir=Path("build"), linux_cc=None, compiler_launcher=None, port_release=False,
                          port_lto="off", port_portable=True, port_pgo="off", port_pgo_profile=None,
                          port_large_mode=large_mode)
    linux_build.generate_linux_build(ninja_syntax.Writer(out), sln)
    return out.getvalue()


def statements(graph: str):
    """the graph's build statements, each with its indented variables"""
    # (ninja's long lines continue after a "$" at the end of one)
    blocks = re.split(r"\n(?=build )", re.sub(r"\$\n\s*", "", graph))
    return [block for block in blocks if block.startswith("build ")]


@pytest.fixture
def at_the_root(monkeypatch):
    monkeypatch.chdir(ROOT)


def test_a_large_mode_build_builds_and_links_the_library(at_the_root):
    graph = build_graph(True)
    library = "build/linux/libhalo_client.a"
    # the library has its own edge, for the 32-bit Linux target
    edge = [s for s in statements(graph) if s.startswith(f"build {library}: rust_client")]
    assert len(edge) == 1 and "target = i686-unknown-linux-gnu" in edge[0]
    assert "rust/halo-client/src/ffi.rs" in edge[0] and "rust/halo-wire/src/datagram.rs" in edge[0]
    # the adapter is compiled with the define that turns the mode on, and nothing else is
    defined = [s for s in statements(graph) if "-DHALO_LARGE_MODE" in s]
    assert len(defined) == 1 and "port/linux/game/large_mode.c" in defined[0]
    # the game's link names the library and what it needs of the system, and waits for it
    link = [s for s in statements(graph) if s.startswith("build build/linux/halo: linux_link")]
    assert len(link) == 1
    assert library in link[0].split("\n", 1)[0], "the link depends on the library"
    libs = re.search(r"libs = (.*)", link[0]).group(1)
    assert libs.startswith(library) and "-lgcc_s" in libs and "-lSDL3" in libs


def test_a_build_without_the_mode_has_none_of_it(at_the_root):
    graph = build_graph(False)
    assert "halo_client" not in graph and "rust_client" not in graph and "HALO_LARGE_MODE" not in graph


def test_the_library_is_rebuilt_only_when_what_it_is_built_from_changed():
    files = rust_client.source_files()
    names = {f.as_posix() for f in files}
    assert {"rust/halo-client/Cargo.toml", "rust/halo-client/Cargo.lock", "rust/halo-client/src/ffi.rs",
            "rust/halo-wire/src/datagram.rs", "rust/halo-match-driver/src/module_bindings/mod.rs",
            "rust/halo-match-driver/src/root_bindings/mod.rs", "rust/halo-client/src/browser.rs"} <= names
    assert not any("/target/" in name for name in names)


def test_the_library_is_a_release_build_against_the_lock_file():
    command = rust_client.cargo_command("i686-unknown-linux-gnu")
    assert command[1:4] == ["build", "--release", "--locked"] and "i686-unknown-linux-gnu" in command


def test_each_targets_library_is_where_cargo_leaves_it():
    assert rust_client.library_path("i686-unknown-linux-gnu", Path("r")) == Path(
        "r/rust/halo-client/target/i686-unknown-linux-gnu/release/libhalo_client.a")
    assert rust_client.library_path("i686-pc-windows-msvc", Path("r")).name == "halo_client.lib"
    assert "gcc_s" in rust_client.system_libraries("i686-unknown-linux-gnu")
    assert "ws2_32" in rust_client.system_libraries("i686-pc-windows-msvc")


def test_the_rule_is_defined_once_when_two_generators_ask_for_it():
    # the Linux and the Windows generators both run on Windows
    out = io.StringIO()
    writer = ninja_syntax.Writer(out)
    linux_build.rust_library_rule(writer)
    linux_build.rust_library_rule(writer)
    assert out.getvalue().count("rule rust_client") == 1
