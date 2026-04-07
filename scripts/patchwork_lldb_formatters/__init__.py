import sys
from typing import Callable, Generator, List, Optional

import hashlib
from lldb import SBDebugger, SBError, SBTarget, SBType, SBTypeCategory, SBTypeNameSpecifier, SBTypeSummary, SBValue, eTypeOptionCascade  # pyright: ignore[reportMissingModuleSource]

INVALID_SUMMARY = "<invalid>"  # Invalid pointer, uninitialized objects, etc.

# Rust-specific summaries
RUST_HISTORY_REF_PATTERN: str = "^(::)?(patchwork_rust|patchwork_rust_core)(::helpers::history_ref)?::HistoryRef$"
RUST_DOCUMENT_ID_PATTERN: str = "^(::)?(samod|samod_core)(::document_id)?::DocumentId$"
RUST_CHANGE_HASH_PATTERN: str = "^(::)?automerge(::types)?::ChangeHash$"
RUST_UUID_PATTERN: str = "^(::)?uuid::Uuid$"


_BASE58_ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def _to_byte_list(valobj: Optional[SBValue]) -> Optional[List[int]]:
    if valobj is None or not valobj.IsValid():
        return None
    data = valobj.GetData()
    if not data or data.size == 0:
        return None
    result: List[int] = []
    error = SBError()
    for i in range(data.size):
        result.append(data.GetUnsignedInt8(error, i))
        if error.Fail():
            return None
    return result


def _get_newtype_first_field(valobj: Optional[SBValue]) -> Optional[SBValue]:
    if valobj is None or not valobj.IsValid():
        return None

    for name in ("0", "__0", "value", "inner"):
        child = valobj.GetChildMemberWithName(name)
        if child and child.IsValid():
            return child

    for i in range(valobj.GetNumChildren()):
        child = valobj.GetChildAtIndex(i)
        if child and child.IsValid():
            return child
    return None


def _extract_fixed_bytes(valobj: Optional[SBValue], expected_size: int) -> Optional[List[int]]:
    if valobj is None or not valobj.IsValid():
        return None

    # try progressively unwrapping tuple/newtype wrappers.
    candidate = valobj
    best_prefix: Optional[List[int]] = None
    for _ in range(5):
        byte_list = _to_byte_list(candidate)
        if byte_list is not None:
            if len(byte_list) == expected_size:
                return byte_list
            if len(byte_list) > expected_size and best_prefix is None:
                best_prefix = byte_list[:expected_size]
        candidate = _get_newtype_first_field(candidate)
        if candidate is None:
            break
    return best_prefix


def _base58_encode(payload: bytes) -> str:
    if len(payload) == 0:
        return ""

    zeros = 0
    for b in payload:
        if b == 0:
            zeros += 1
        else:
            break

    num = int.from_bytes(payload, byteorder="big", signed=False)
    encoded = ""
    while num > 0:
        num, remainder = divmod(num, 58)
        encoded = _BASE58_ALPHABET[remainder] + encoded
    return ("1" * zeros) + encoded


def _base58check_encode(payload: bytes) -> str:
    checksum = hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]
    return _base58_encode(payload + checksum)


def _normalize_summary(
    valobj: Optional[SBValue],
    internal_dict,
    fallback_provider: Optional[Callable[[SBValue, object], str]] = None,
) -> str:
    if valobj is None or not valobj.IsValid():
        return INVALID_SUMMARY
    summary = valobj.GetSummary()
    if summary is not None and summary != "":
        return summary
    if fallback_provider is not None:
        return fallback_provider(valobj, internal_dict)
    return INVALID_SUMMARY


def _extract_vec_children(valobj: Optional[SBValue]) -> List[SBValue]:
    if valobj is None or not valobj.IsValid():
        return []
    # if not synthetic, instantiate a StdVecSyntheticProvider and get the children from that
    if not valobj.IsSynthetic():
        std_vec_provider = StdVecSyntheticProvider(valobj, {})
        count = std_vec_provider.num_children()
        if count == 0:
            print("NO CHILDREN for history ref")
            return []
        children_list: List[SBValue] = []
        for i in range(count):
            child = std_vec_provider.get_child_at_index(i)
            children_list.append(child)
        return children_list

    count = valobj.GetNumChildren()
    if count == 0:
        print("NO CHILDREN for history ref")
        return []

    children: List[SBValue] = []
    indexed_children: List[SBValue] = []
    # print(f"COUNT: {count} for history ref")
    for i in range(count):
        child = valobj.GetChildAtIndex(i)
        # print(f"CHILD: {child}")
        if child is None or not child.IsValid():
            print(f"INVALID CHILD FOR HISTORY REF AT INDEX {i}")
            continue
        children.append(child)
        child_name = child.GetName() or ""
        # Rust synthetic Vec children are usually indexed entries, while
        # non-synthetic children are internals like "buf"/"len"/"cap".
        if (child_name.startswith("[") and child_name.endswith("]")) or child_name.isdigit():
            indexed_children.append(child)
    if len(indexed_children) > 0:
        return indexed_children
    if len(children) > 0:
        print("NO children at all!!!!!")
        return []
    return children


def RustDocumentIdSummaryProvider(valobj: SBValue, internal_dict):
    raw_bytes = _extract_fixed_bytes(valobj, 16)
    if raw_bytes is None or len(raw_bytes) != 16:
        return INVALID_SUMMARY
    return _base58check_encode(bytes(raw_bytes))


def RustChangeHashSummaryProvider(valobj: SBValue, internal_dict):
    raw_bytes = _extract_fixed_bytes(valobj, 32)
    if raw_bytes is None or len(raw_bytes) != 32:
        return INVALID_SUMMARY
    return bytes(raw_bytes).hex()


def RustHistoryRefSummaryProvider(valobj: SBValue, internal_dict):
    if valobj is None or not valobj.IsValid():
        print("INVALID HISTORY REF, NOT A VALID OBJECT")
        return INVALID_SUMMARY

    branch = valobj.GetChildMemberWithName("branch")
    heads = valobj.GetChildMemberWithName("heads")
    if branch is None or not branch.IsValid() or heads is None or not heads.IsValid():
        print("INVALID HISTORY REF, BRANCH OR HEADS ARE NOT VALID")
        return INVALID_SUMMARY

    branch_summary = _normalize_summary(branch, internal_dict, RustDocumentIdSummaryProvider)
    if branch_summary == INVALID_SUMMARY:
        print("INVALID HISTORY REF, BRANCH SUMMARY IS INVALID")
        return INVALID_SUMMARY

    head_children = _extract_vec_children(heads)
    if len(head_children) == 0:
        print("INVALID HISTORY REF, NO HEADS")
        # HistoryRef::Display returns fmt::Error when heads are empty.
        return INVALID_SUMMARY

    head_summaries: List[str] = []
    for i, head in enumerate(head_children):
        head_summary = _normalize_summary(head, internal_dict, RustChangeHashSummaryProvider)
        if head_summary == INVALID_SUMMARY:
            print(f"INVALID HISTORY REF, HEAD SUMMARY IS INVALID FOR HEAD {i}")
            return INVALID_SUMMARY
        head_summaries.append(head_summary)

    return f"{branch_summary}+{'.'.join(head_summaries)}"


def RustUuidSummaryProvider(valobj: SBValue, internal_dict):
    # uuid::Uuid Display delegates to LowerHex over the hyphenated form.
    raw_bytes = _extract_fixed_bytes(valobj, 16)
    if raw_bytes is None or len(raw_bytes) != 16:
        return INVALID_SUMMARY
    hex_str = bytes(raw_bytes).hex()
    return f"{hex_str[0:8]}-{hex_str[8:12]}-{hex_str[12:16]}-{hex_str[16:20]}-{hex_str[20:32]}"


SUMMARY_PROVIDERS: dict[str, object] = {
    RUST_HISTORY_REF_PATTERN: RustHistoryRefSummaryProvider,
    RUST_DOCUMENT_ID_PATTERN: RustDocumentIdSummaryProvider,
    RUST_CHANGE_HASH_PATTERN: RustChangeHashSummaryProvider,
    RUST_UUID_PATTERN: RustUuidSummaryProvider,
}




def attach_summary_to_type(module, category: SBTypeCategory, type_name, real_summary_fn, is_regex=False, real_fn_name: Optional[str] = None):
    if not real_fn_name:
        real_fn_name = str(real_summary_fn.__qualname__)
    def __spfunc(valobj, dict):
        try:
            return real_summary_fn(valobj, dict)
        except Exception as e:
            err_msg = "ERROR in " + real_fn_name + ": " + str(e)
            print(err_msg)
            print(f"Traceback: {e}")
            return f"<{err_msg}>"

    # LLDB accesses summary fn's by name, so we need to create a unique one.
    __spfunc.__name__ = "__spfunc__" + real_fn_name.replace(".", "_")
    setattr(module, __spfunc.__name__, __spfunc)

    summary = SBTypeSummary.CreateWithFunctionName(__name__ + "." + __spfunc.__name__)
    summary.SetOptions(eTypeOptionCascade)
    if not category.AddTypeSummary(SBTypeNameSpecifier(type_name, is_regex), summary):
        print(f"Failed to add summary for {type_name}")

def remove_all_summary_providers(category: SBTypeCategory, SUMMARY_PROVIDERS):
    for key in SUMMARY_PROVIDERS:
        try:
            if category.DeleteTypeSummary(SBTypeNameSpecifier(key, True)):
                pass
                # print_trace(f"Deleted summary for {key}")
            else:
                pass
                # print_trace(f"No summary found for {key}")
        except Exception as e:
            print(f"EXCEPTION WHILE REMOVING {key}: " + str(e))


def register_all_synth_providers(
    module, category: SBTypeCategory, debugger: SBDebugger, SUMMARY_PROVIDERS
):
    remove_all_summary_providers(category, SUMMARY_PROVIDERS)
    for key in SUMMARY_PROVIDERS:
        try:
            attach_summary_to_type(module, category, key, SUMMARY_PROVIDERS[key], True)
        except Exception as e:
            print("EXCEPTION: " + str(e))

def install_patchwork_visualizers(debugger: SBDebugger, dict):
    cpp_category = debugger.GetDefaultCategory()
    rust_category = debugger.GetCategory("Rust")
    if not rust_category:
        debugger.HandleCommand("type category enable Rust")
        rust_category = debugger.GetCategory("Rust")
        
    if not rust_category:
        print("Failed to enable Patchwork category, using C++ category instead")
        rust_category = cpp_category
            
    module = sys.modules[__name__]
    register_all_synth_providers(module, rust_category, debugger, SUMMARY_PROVIDERS)


def __lldb_init_module(debugger: SBDebugger, dict):
    install_patchwork_visualizers(debugger, dict)


# Copied from Rust providers; used by HistoryRefSummaryProvider if these aren't already available
def get_template_args(type_name: str) -> Generator[str, None, None]:
    """
    Takes a type name `T<A, tuple$<B, C>, D>` and returns a list of its generic args
    `["A", "tuple$<B, C>", "D"]`.

    String-based replacement for LLDB's `SBType.template_args`, as LLDB is currently unable to
    populate this field for targets with PDB debug info. Also useful for manually altering the type
    name of generics (e.g. `Vec<ref$<str$> >` -> `Vec<&str>`).

    Each element of the returned list can be looked up for its `SBType` value via
    `SBTarget.FindFirstType()`
    """
    level = 0
    start = 0
    for i, c in enumerate(type_name):
        if c == "<":
            level += 1
            if level == 1:
                start = i + 1
        elif c == ">":
            level -= 1
            if level == 0:
                yield type_name[start:i].strip()
        elif c == "," and level == 1:
            yield type_name[start:i].strip()
            start = i + 1


def unwrap_unique_or_non_null(unique_or_nonnull: SBValue) -> SBValue:
    # BACKCOMPAT: rust 1.32
    # https://github.com/rust-lang/rust/commit/7a0911528058e87d22ea305695f4047572c5e067
    # BACKCOMPAT: rust 1.60
    # https://github.com/rust-lang/rust/commit/2a91eeac1a2d27dd3de1bf55515d765da20fd86f
    ptr = unique_or_nonnull.GetChildMemberWithName("pointer")
    return ptr if ptr.TypeIsPointerType() else ptr.GetChildAtIndex(0)


MSVC_PTR_PREFIX: List[str] = ["ref$<", "ref_mut$<", "ptr_const$<", "ptr_mut$<"]


def resolve_msvc_template_arg(arg_name: str, target: SBTarget) -> SBType:
    """
    RECURSIVE when arrays or references are nested (e.g. `ref$<ref$<u8> >`, `array$<ref$<u8> >`)

    Takes the template arg's name (likely from `get_template_args`) and finds/creates its
    corresponding SBType.

    For non-reference/pointer/array types this is identical to calling
    `target.FindFirstType(arg_name)`

    LLDB internally interprets refs, pointers, and arrays C-style (`&u8` -> `u8 *`,
    `*const u8` -> `u8 *`, `[u8; 5]` -> `u8 [5]`). Looking up these names still doesn't work in the
    current version of LLDB, so instead the types are generated via `base_type.GetPointerType()` and
    `base_type.GetArrayType()`, which bypass the PDB file and ask clang directly for the type node.
    """
    result = target.FindFirstType(arg_name)

    if result.IsValid():
        return result

    for prefix in MSVC_PTR_PREFIX:
        if arg_name.startswith(prefix):
            arg_name = arg_name[len(prefix) : -1].strip()

            result = resolve_msvc_template_arg(arg_name, target)
            return result.GetPointerType()

    if arg_name.startswith("array$<"):
        arg_name = arg_name[7:-1].strip()

        template_args = get_template_args(arg_name)

        element_name = next(template_args)
        length = next(template_args)

        result = resolve_msvc_template_arg(element_name, target)

        return result.GetArrayType(int(length))

    return result


class StdVecSyntheticProvider:
    """Pretty-printer for alloc::vec::Vec<T>

    struct Vec<T> { buf: RawVec<T>, len: usize }
    rust 1.75: struct RawVec<T> { ptr: Unique<T>, cap: usize, ... }
    rust 1.76: struct RawVec<T> { ptr: Unique<T>, cap: Cap(usize), ... }
    rust 1.31.1: struct Unique<T: ?Sized> { pointer: NonZero<*const T>, ... }
    rust 1.33.0: struct Unique<T: ?Sized> { pointer: *const T, ... }
    rust 1.62.0: struct Unique<T: ?Sized> { pointer: NonNull<T>, ... }
    struct NonZero<T>(T)
    struct NonNull<T> { pointer: *const T }
    """

    def __init__(self, valobj: SBValue, _dict):
        # logger = Logger.Logger()
        # logger >> "[StdVecSyntheticProvider] for " + str(valobj.GetName())
        self.valobj = valobj
        self.element_type: SBType | None = None
        self.update()

    def num_children(self) -> int:
        return self.length

    def get_child_index(self, name: str) -> int:
        index = name.lstrip("[").rstrip("]")
        if index.isdigit():
            return int(index)
        else:
            return -1

    def get_child_at_index(self, index: int) -> SBValue:
        if not self.element_type or not self.data_ptr:
            return SBValue()
        start = self.data_ptr.GetValueAsUnsigned()
        address = start + index * self.element_type_size
        element = self.data_ptr.CreateValueFromAddress("[%s]" % index, address, self.element_type)
        return element

    def update(self):
        self.length = self.valobj.GetChildMemberWithName("len").GetValueAsUnsigned()
        self.buf = self.valobj.GetChildMemberWithName("buf").GetChildMemberWithName("inner")

        self.data_ptr = unwrap_unique_or_non_null(self.buf.GetChildMemberWithName("ptr"))

        self.element_type = self.valobj.GetType().GetTemplateArgumentType(0)

        if not self.element_type.IsValid():
            arg_name = next(get_template_args(self.valobj.GetTypeName()))

            self.element_type = resolve_msvc_template_arg(arg_name, self.valobj.target)

        self.element_type_size = self.element_type.GetByteSize()

    def has_children(self) -> bool:
        return True
