"""Writes key_names.rs: which of winit's keys each of WaOS's key numbers is.

WaOS numbers keys as Flutter does (`wasdk::keys`), and both Flutter and winit take their names
from the W3C's UI Events `code` and `key` values, so a key is matched by its name. Run from this
folder, with wasdk's keys.rs:

    python3 key_names.py <waos>/sdk/wasdk/src/keys.rs > key_names.rs
"""

import re
import sys

KEYBOARD = "../../keyboard.rs"

# winit's names that differ from Flutter's for the same key.
PHYSICAL_RENAMED = {"SuperLeft": "META_LEFT", "SuperRight": "META_RIGHT"}
# Flutter names a modifier by its side where winit names the key and gives the side apart.
LOGICAL_RENAMED = {
    "Super": ["META", "META_LEFT", "META_RIGHT"],
    "Control": ["CONTROL", "CONTROL_LEFT", "CONTROL_RIGHT"],
    "Shift": ["SHIFT", "SHIFT_LEFT", "SHIFT_RIGHT"],
    "Alt": ["ALT", "ALT_LEFT", "ALT_RIGHT"],
    "Enter": ["ENTER", "NUMPAD_ENTER"],
}
# winit's older name for the key it now names Super, which Flutter names Meta.
LOGICAL_SKIPPED = {"Meta"}


def constants(module):
    return [name for name, _ in re.findall(r"pub const (\w+): u64 = (0x[0-9a-fA-F]+);", module)]


def wasdk_keys(path):
    source = open(path).read()
    logical_at, physical_at = source.index("pub mod logical"), source.index("pub mod physical")
    if logical_at < physical_at:
        return constants(source[physical_at:]), constants(source[logical_at:physical_at])
    return constants(source[physical_at:logical_at]), constants(source[logical_at:])


def variants(enum):
    body = re.search(r"pub enum " + enum + r" \{(.*?)\n\}", open(KEYBOARD).read(), re.S).group(1)
    return re.findall(r"^\s{4}([A-Z]\w*),", body, re.M)


def flutter_names(variant):
    snake = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", variant).upper()
    return [snake, re.sub(r"_(\d)", r"\1", snake), snake.replace("_", "")]


def arms(enum, winit_names, wasdk_names, module, renamed, skipped=frozenset()):
    lines = []
    for variant in winit_names:
        if variant in skipped:
            continue
        names = renamed.get(variant, flutter_names(variant))
        names = [names] if isinstance(names, str) else names
        found = [name for name in names if name in wasdk_names]
        if variant in renamed:
            lines += [f"        {module}::{name} => {enum}::{variant}," for name in found]
        elif found:
            lines.append(f"        {module}::{found[0]} => {enum}::{variant},")
    return "\n".join(lines)


physical, logical = wasdk_keys(sys.argv[1])
print(f"""//! Which of winit's keys each of WaOS's key numbers is. Written by `key_names.py`: run it again
//! rather than edit.

use wasdk::keys::{{logical, physical}};

use crate::keyboard::{{KeyCode, NamedKey}};

/// The key at the place `physical` numbers, where winit has a name for it.
pub(super) fn key_code(physical: u64) -> Option<KeyCode> {{
    Some(match physical {{
{arms("KeyCode", variants("KeyCode"), physical, "physical", PHYSICAL_RENAMED)}
        _ => return None,
    }})
}}

/// The key `logical` numbers, where it is one winit names and not a character.
pub(super) fn named_key(logical: u64) -> Option<NamedKey> {{
    Some(match logical {{
{arms("NamedKey", variants("NamedKey"), logical, "logical", LOGICAL_RENAMED, LOGICAL_SKIPPED)}
        _ => return None,
    }})
}}""")
