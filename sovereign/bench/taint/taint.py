#!/usr/bin/env python3
"""Untangled-wire core: intraprocedural taint/def-use over tree-sitter-rust.

The primitive naive approaches lack: given a panic SINK (`.unwrap()` receiver,
an index/slice subscript, a `step_by` arg), backward-slice its value through
the function's def-use chains to its ROOTS, and classify:

  untrusted  - a root is an untrusted-typed parameter, or the value derives
               from a deserialization/request-body SOURCE call
  param      - a root is some function parameter (tainted, type unknown)
  internal   - every root is a literal / const / internal constructor

This is the intraprocedural half of IFDS taint. Rung 2 (interproc) walks
SCIP callers to carry `param` taint back to a source.
"""
import re
from tree_sitter import Language, Parser
import tree_sitter_rust as tsr

LANG = Language(tsr.language())
PARSER = Parser(LANG)

def txt(node, src):
    return src[node.start_byte:node.end_byte].decode("utf8", "ignore")

# ---- source (untrusted) models -------------------------------------------
UNTRUSTED_TYPE = re.compile(
    r"\bJson<|\bQuery<|\bPath<|\bBytes\b|\bBytesMut\b|&\s*\[\s*u8\s*\]|"
    r"\bserde_json::Value\b|\bValue\b|\bString\b|&\s*str\b|\bVec\s*<\s*u8")
UNTRUSTED_NAME = re.compile(
    r"\b(req|request|body|payload|input|msg|message|text|raw|data|content|"
    r"prompt|query|user|arg|args|params|json|bytes|buf)\b", re.I)
SOURCE_CALL = re.compile(
    r"from_slice|from_str|from_reader|from_utf8|deserialize|::from_value|"
    r"\.bytes\(\)|\.body\(\)|\.text\(\)|serde_json::from|serde_yaml::from|"
    r"toml::from")

def param_is_untrusted(name, type_str):
    return bool(UNTRUSTED_TYPE.search(type_str)) or bool(UNTRUSTED_NAME.fullmatch(name)) \
        or bool(UNTRUSTED_NAME.search(name))

# ---- data-position identifier walk (the precision crux) ------------------
def data_idents(node, src):
    """Yield identifiers that appear in VALUE position under `node`
    (skips method names, field names, free-fn names, type paths)."""
    t = node.type
    if t == "identifier":
        yield txt(node, src); return
    if t in ("integer_literal","float_literal","string_literal","char_literal",
             "boolean_literal","raw_string_literal","line_comment","block_comment"):
        return
    if t == "call_expression":
        fn = node.child_by_field_name("function")
        args = node.child_by_field_name("arguments")
        if fn is not None and fn.type in ("field_expression","generic_function"):
            # method call: receiver is data, method name is not
            recv = fn
            if fn.type == "generic_function":
                recv = fn.child_by_field_name("function") or fn
            if recv is not None and recv.type == "field_expression":
                v = recv.child_by_field_name("value")
                if v is not None:
                    yield from data_idents(v, src)
            elif recv is not None:
                yield from data_idents(recv, src)
        # free fn call: fn name is not data (skip); a var holding a closure is rare
        if args is not None:
            for a in args.named_children:
                yield from data_idents(a, src)
        return
    if t == "field_expression":
        v = node.child_by_field_name("value")
        if v is not None:
            yield from data_idents(v, src)
        return
    if t == "scoped_identifier" or t == "type_identifier" or t == "scoped_type_identifier":
        return  # a path like foo::Bar — not local data
    # index, binary, unary, reference, paren, try(?), await, range, struct,
    # array, tuple, macro, closure-body etc: recurse all named children
    for c in node.named_children:
        yield from data_idents(c, src)

# ---- per-function analysis ------------------------------------------------
class Fn:
    __slots__=("name","params","untrusted_params","letmap","body","node","start_row")
    def __init__(self): self.letmap={}; self.params=set(); self.untrusted_params=set()

def collect_params(fn_node, src):
    params, untrusted = set(), set()
    p = fn_node.child_by_field_name("parameters")
    if p is None: return params, untrusted
    for c in p.named_children:
        if c.type == "self_parameter": continue
        if c.type != "parameter": continue
        pat = c.child_by_field_name("pattern")
        typ = c.child_by_field_name("type")
        type_str = txt(typ, src) if typ is not None else ""
        # pattern may be identifier / mut_pattern(identifier) / tuple/struct
        for idn in idents_in_pattern(pat, src):
            params.add(idn)
            if param_is_untrusted(idn, type_str):
                untrusted.add(idn)
    return params, untrusted

def idents_in_pattern(node, src):
    if node is None: return
    if node.type == "identifier":
        yield txt(node, src); return
    for c in node.named_children:
        yield from idents_in_pattern(c, src)

def params_ordered(fn_node, src):
    """Ordered [(name, type_str)] excluding self — for positional interproc."""
    out=[]
    p=fn_node.child_by_field_name("parameters")
    if p is None: return out
    for c in p.named_children:
        if c.type=="self_parameter" or c.type!="parameter": continue
        pat=c.child_by_field_name("pattern"); typ=c.child_by_field_name("type")
        type_str=txt(typ,src) if typ is not None else ""
        names=list(idents_in_pattern(pat, src))
        out.append((names[0] if names else "_", type_str))
    return out

def build_letmap(body, src):
    """var name -> value expr node (last binding wins; simple approximation)."""
    m = {}
    def walk(n):
        if n.type == "let_declaration":
            pat = n.child_by_field_name("pattern")
            val = n.child_by_field_name("value")
            if val is not None and pat is not None:
                for idn in idents_in_pattern(pat, src):
                    m[idn] = val
        for c in n.named_children:
            # don't descend into nested fn/closure defs for the letmap
            if c.type in ("function_item","closure_expression"): continue
            walk(c)
    walk(body)
    return m

def provenance(expr, letmap, params, untrusted, src, depth=0, seen=None):
    """Return set of root tags reached by backward slicing `expr`."""
    if seen is None: seen=set()
    roots=set()
    # source call anywhere in the expr subtree?
    if SOURCE_CALL.search(txt(expr, src)):
        roots.add(("source_call",))
    for name in data_idents(expr, src):
        if name in untrusted:
            roots.add(("untrusted_param", name)); roots.add(("param", name))
        elif name in params:
            roots.add(("param", name))
        elif name in letmap and (name, depth) not in seen and depth < 40:
            seen.add((name, depth))
            roots |= provenance(letmap[name], letmap, params, untrusted, src, depth+1, seen)
        else:
            roots.add(("free", name))
    return roots

def classify(roots):
    if any(r[0] in ("untrusted_param","source_call") for r in roots): return "untrusted"
    if any(r[0]=="param" for r in roots): return "param"
    return "internal"

# ---- sinks ---------------------------------------------------------------
def find_sinks(fn_node, src):
    """Yield (kind, sink_expr_node, line1) for panic sinks in this fn body."""
    body = fn_node.child_by_field_name("body")
    if body is None: return
    out=[]
    def walk(n):
        if n.type in ("function_item","closure_expression") and n is not fn_node:
            return  # nested fn — belongs to its own analysis
        if n.type == "call_expression":
            fn = n.child_by_field_name("function")
            if fn is not None and fn.type in ("field_expression","generic_function"):
                fe = fn if fn.type=="field_expression" else fn.child_by_field_name("function")
                if fe is not None and fe.type=="field_expression":
                    field = fe.child_by_field_name("field")
                    fname = txt(field, src) if field is not None else ""
                    recv = fe.child_by_field_name("value")
                    if fname in ("unwrap","expect") and recv is not None:
                        out.append((fname, recv, n.start_point[0]+1))
                    elif fname == "step_by":
                        args = n.child_by_field_name("arguments")
                        if args is not None and args.named_children:
                            out.append(("step_by", args.named_children[0], n.start_point[0]+1))
        elif n.type == "index_expression":
            # a[b] -> the subscript (2nd child expr) is the index; slice a[x..y] too
            kids=[c for c in n.named_children]
            if len(kids)>=2:
                out.append(("index", kids[1], n.start_point[0]+1))
        for c in n.named_children:
            walk(c)
    walk(body)
    return out

# ---- file driver ---------------------------------------------------------
def _attr_says_test(node, src):
    prev = node.prev_named_sibling
    while prev is not None and prev.type in ("attribute_item","line_comment","block_comment"):
        if prev.type=="attribute_item" and "test" in txt(prev, src):
            return True
        prev = prev.prev_named_sibling
    return False

def is_test_function(fn_node, src):
    if _attr_says_test(fn_node, src):
        return True
    a=fn_node.parent
    while a is not None:
        if a.type=="mod_item":
            nm=a.child_by_field_name("name")
            if nm is not None and "test" in txt(nm, src):
                return True
            if _attr_says_test(a, src):   # #[cfg(test)] mod ...
                return True
        a=a.parent
    return False

def walk_functions(root):
    stack=[root]
    while stack:
        n=stack.pop()
        if n.type=="function_item":
            yield n
        for c in n.children:
            stack.append(c)

def analyze_file(path, rel=None):
    with open(path,"rb") as f: src=f.read()
    tree=PARSER.parse(src)
    results=[]  # (rel, fn_name, kind, line1, cls, roots, sink_text)
    rel = rel or path
    for fn_node in walk_functions(tree.root_node):
        if is_test_function(fn_node, src): continue
        nm=fn_node.child_by_field_name("name")
        fn_name= txt(nm, src) if nm is not None else "?"
        params, untrusted = collect_params(fn_node, src)
        body=fn_node.child_by_field_name("body")
        if body is None: continue
        letmap=build_letmap(body, src)
        for kind, sink, line1 in find_sinks(fn_node, src):
            roots=provenance(sink, letmap, params, untrusted, src)
            results.append((rel, fn_name, kind, line1, classify(roots),
                            roots, txt(sink, src)[:60].replace("\n"," ")))
    return results

if __name__=="__main__":
    import sys
    path = sys.argv[1] if len(sys.argv)>1 else \
        "/home/alexbryan/dev/commonwealth-ai/commonwealth/crates/commonwealth-api/src/frontdoor.rs"
    res=analyze_file(path)
    from collections import Counter
    c=Counter(r[4] for r in res)
    print(f"{path.split('/')[-1]}: {len(res)} sinks (non-test)  ->  {dict(c)}\n")
    print("--- UNTRUSTED-tainted sinks (the wire-heads) ---")
    for rel,fn_name,kind,line1,cls,roots,stext in res:
        if cls=="untrusted":
            rr=sorted({r[1] for r in roots if len(r)>1 and r[0] in ('untrusted_param','param')})
            src_call = any(r[0]=="source_call" for r in roots)
            print(f"  {kind:7} L{line1:<5} {fn_name}()  sink=`{stext}`")
            print(f"          roots={rr}  source_call={src_call}")
