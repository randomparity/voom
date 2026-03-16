"""MessagePack serialization compatible with Rust rmp_serde format.

Rust's rmp_serde serializes enums with external tagging:
  Event::FileIntrospected(e) → {"FileIntrospected": {"file": {...}}}

PathBuf is serialized as a string.
Option<T> with None → MessagePack nil.
serde_json::Value maps directly to equivalent MessagePack types.
"""

import umsgpack


def unpack(data: bytes) -> dict:
    """Deserialize MessagePack bytes to a Python dict.

    Handles rmp_serde conventions: enum variants as 1-element maps,
    PathBuf as strings, etc.
    """
    try:
        return umsgpack.unpackb(data)
    except RecursionError:
        raise ValueError("msgpack payload nesting depth exceeds limit")


def pack(obj: dict) -> bytes:
    """Serialize a Python dict to MessagePack bytes.

    The caller must structure the dict to match rmp_serde's external tagging
    convention for Rust enums.
    """
    return umsgpack.packb(obj)


def unpack_event(data: bytes) -> tuple[str, dict]:
    """Unpack a MessagePack-encoded Rust Event enum.

    Returns (variant_name, payload_dict).
    E.g., for FileIntrospected: ("FileIntrospected", {"file": {...}})
    """
    raw = unpack(data)
    if not isinstance(raw, dict) or len(raw) != 1:
        raise ValueError(f"Expected externally-tagged enum (1-element map), got: {type(raw)}")
    variant = next(iter(raw))
    return variant, raw[variant]


def pack_event(variant: str, payload: dict) -> bytes:
    """Pack a Python dict as a MessagePack-encoded Rust Event enum.

    Uses rmp_serde external tagging: {"VariantName": {payload...}}
    """
    return pack({variant: payload})
