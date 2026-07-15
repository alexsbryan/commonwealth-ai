#!/usr/bin/env python3
"""Sink-guard analysis — is the index/bound/divisor provably safe?

Three verdicts (no soft-fail, mirrors the fact-base Corroborated/Drift/Unverifiable):
  SAFE       every offset is provably in-bounds / non-zero / char-boundary
  UNGUARDED  an offset has an unguarded panic shape (underflow `-`, zero div `/`)
  UNCERTAIN  can't decide structurally -> hand to the verifier (never dropped)

Guard signals recognised on an offset's DERIVATION (following def-use):
  find      derives from .find/.rfind/.position     -> str char-boundary + < len
  clamp     wrapped/contains .min(<len-ish>)         -> upper-bounded
  bufread   `n` where n = .read(&mut BASE), slice BASE[..n]  -> n <= BASE.len()
  satsub    saturating_sub / checked_sub             -> no underflow
  maxge1    outer .max(K>=1)                          -> non-zero (for step/div)
  literal   integer literal                          -> known
Risk signals:
  underflow binary `-` not guarded above             -> usize wrap -> OOB
  zerodiv   top-level integer `/` not outer-.max     -> step/div can be 0
"""
import re
from taint import txt

def _expand(node, letmap, src, depth=0, seen=None):
    """Concatenated text of node + transitive let-definitions of its idents."""
    if seen is None: seen=set()
    parts=[txt(node,src)]
    if depth>12: return " ".join(parts)
    ids=set()
    st=[node]
    while st:
        n=st.pop()
        if n.type=="identifier": ids.add(txt(n,src))
        for c in n.named_children: st.append(c)
    for v in ids:
        if v in letmap and (v,depth) not in seen:
            seen.add((v,depth))
            parts.append(_expand(letmap[v], letmap, src, depth+1, seen))
    return " ".join(parts)

def outer_max_ge1(node, src):
    n=node
    while n.type=="parenthesized_expression" and n.named_children: n=n.named_children[0]
    if n.type=="call_expression":
        fn=n.child_by_field_name("function")
        if fn is not None and fn.type=="field_expression":
            fld=fn.child_by_field_name("field")
            if fld is not None and txt(fld,src)=="max":
                a=n.child_by_field_name("arguments")
                if a is not None and a.named_children:
                    x=a.named_children[0]
                    if x.type=="integer_literal":
                        return int(re.sub(r"[^0-9]","",txt(x,src)) or "1")>=1
                    return True
    return False

def has_top_div(node, src):
    n=node
    while n.type=="parenthesized_expression" and n.named_children: n=n.named_children[0]
    if n.type=="binary_expression":
        return any((not c.is_named and txt(c,src)=="/") for c in n.children)
    return False

def _offset_class(expr, letmap, src, base_text):
    deriv=_expand(expr, letmap, src)
    if re.search(r"\.(find|rfind|position)\s*\(", deriv):            return "find"
    if re.search(r"saturating_sub|checked_sub", deriv):             return "satsub"
    if re.search(r"\.min\s*\(", deriv):                             return "clamp"
    if base_text and re.search(r"\.read\s*\(\s*&\s*mut\s+"+re.escape(base_text.strip("&").strip()), deriv):
        return "bufread"
    if re.search(r"\.read\s*\(", deriv) and base_text and base_text.strip("&").strip() in deriv:
        return "bufread"
    if expr.type=="integer_literal":                                return "literal"
    # risk shapes (only when not guarded above)
    if _has_unguarded_sub(expr, src):                               return "underflow"
    return "unknown"

def _has_unguarded_sub(node, src):
    if node.type=="call_expression":
        t=txt(node,src)
        if "saturating_sub" in t or "checked_sub" in t or ".min(" in t: return False
    if node.type=="binary_expression":
        if any((not c.is_named and txt(c,src)=="-") for c in node.children): return True
    return any(_has_unguarded_sub(c,src) for c in node.named_children)

def dominating_len_guard(sink_node, base_text, src):
    """True if a dominating if/while/match condition checks the base's length
    (a control-flow guard the data-flow pass can't see). Also picks up early
    guard-and-return siblings that reference `<base>.len()/is_empty()`."""
    if not base_text: return False
    base=base_text.strip().strip("&").strip()
    needles=[f"{base}.len()", f"{base}.is_empty()"]
    a=sink_node.parent
    while a is not None:
        if a.type in ("if_expression","while_expression","match_expression","match_arm"):
            cond=a.child_by_field_name("condition") or a.child_by_field_name("value")
            if cond is not None:
                ct=txt(cond,src)
                if any(nd in ct for nd in needles): return True
        # scan preceding siblings for a guard-and-return (if x.is_empty(){return})
        if a.type=="block":
            for c in a.named_children:
                if c.start_point[0] >= sink_node.start_point[0]: break
                if c.type=="expression_statement" or c.type=="if_expression":
                    tc=txt(c,src)
                    if any(nd in tc for nd in needles) and ("return" in tc or "break" in tc
                                                            or "continue" in tc or "?" in tc):
                        return True
        if a.type=="function_item": break
        a=a.parent
    return False

_COMP=re.compile(r'[<>!]=|[<>]|==')
def _idents(node, src, out):
    if node.type=="identifier": out.add(txt(node,src))
    for c in node.named_children: _idents(c,src,out)
def _underflow_vars(offs, src):
    s=set()
    for o in offs: _idents(o,src,s)
    return s
def _mentions(text, vars):
    return any(re.search(r'\b'+re.escape(v)+r'\b', text) for v in vars)

def _lower_bound(text, vars):   # guards `v - k` when v is used: v>0 / v>=1 / v!=0
    return any(re.search(rf'\b{re.escape(v)}\s*(>\s*0|>=\s*[1-9]|!=\s*0)', text) for v in vars)
def _zero_check(text, vars):    # `||` left operand: v==0 / v.is_empty()
    return any(re.search(rf'\b{re.escape(v)}\s*==\s*0\b', text) or
               re.search(rf'\b{re.escape(v)}\.is_empty\(\)', text) for v in vars)

def positivity_guard(sink_node, offset_vars, src):
    """A short-circuit or dominating comparison that lower-bounds an offset var
    makes the `- k` safe:  `x>0 && arr[x-1]`,  `x==0 || arr[x-1]`,  `if x>0 { arr[x-1] }`."""
    if not offset_vars: return False
    frm=sink_node; a=sink_node.parent
    while a is not None:
        if a.type=="binary_expression":
            ops=[txt(c,src) for c in a.children if not c.is_named]
            nc=a.named_children
            if len(nc)==2 and nc[1].id==frm.id:          # sink in RIGHT operand
                lt=txt(nc[0],src)
                if "&&" in ops and _lower_bound(lt, offset_vars): return True
                if "||" in ops and _zero_check(lt, offset_vars): return True
        if a.type in ("if_expression","while_expression"):
            cond=a.child_by_field_name("condition")
            if cond is not None and _lower_bound(txt(cond,src), offset_vars): return True
        if a.type=="function_item": break
        frm=a; a=a.parent
    # guard-and-return: `if x < 1 { return }` / `if x == 0 { ? }` upstream lower-bounds x
    return _guard_and_return_lb(sink_node, offset_vars, src)

def _guard_and_return_lb(sink_node, offset_vars, src):
    """An early-exit `if <x..> (< 1 | <= 0 | == 0 | .is_empty()) { return/?/break }`
    that mentions an offset var lower-bounds it past the guard (so `x - 1` is safe)."""
    if not offset_vars: return False
    def is_lb_return(txt_):
        if not ("return" in txt_ or "break" in txt_ or "continue" in txt_ or "?" in txt_): return False
        return any(re.search(rf'\b{re.escape(v)}\b[\w.]*\s*(<\s*1\b|<=\s*0\b|==\s*0\b)', txt_)
                   or re.search(rf'\b{re.escape(v)}\b[\w.]*\.is_empty\(\)', txt_) for v in offset_vars)
    a=sink_node.parent
    while a is not None:
        if a.type=="block":
            for c in a.named_children:
                if c.start_point[0] >= sink_node.start_point[0]: break
                if c.type in ("expression_statement","if_expression") and is_lb_return(txt(c,src)):
                    return True
        if a.type=="function_item": break
        a=a.parent
    return False

def loop_bound_guard(sink_node, offs, src):
    """`for j in 1..=n { arr[j-1] }` — offset `v - k` is safe when v is the
    loop var and the range starts at a literal >= k."""
    subs=[]
    for o in offs:
        m=re.search(r'\b([a-z_][a-z0-9_]*)\s*-\s*(\d+)\b', txt(o,src))
        if m: subs.append((m.group(1), int(m.group(2))))
    if not subs: return False
    starts={}
    a=sink_node.parent
    while a is not None:
        if a.type=="for_expression":
            nc=a.named_children
            if len(nc)>=2 and nc[1].type=="range_expression":
                mt=re.match(r'\s*(\d+)\s*\.\.', txt(nc[1],src))
                if mt: starts[txt(nc[0],src)]=int(mt.group(1))
        if a.type=="function_item": break
        a=a.parent
    return bool(subs) and all(v in starts and starts[v]>=k for v,k in subs)

def _bounds_of_index(sink_node):
    """Return (base_node, [offset_nodes]) for an index_expression arr[..]."""
    kids=[c for c in sink_node.named_children]
    if len(kids)<2: return (kids[0] if kids else None, [])
    base, sub = kids[0], kids[1]
    if sub.type=="range_expression":
        return base, [c for c in sub.named_children]     # start/end (0,1, or 2)
    return base, [sub]

# ---- arithmetic underflow: bare `a - b` (usize wraps -> huge / panics in debug) ----
def _sizelike(node, src):
    """Operand plausibly an unsigned integer size (so `-` can underflow)."""
    t=node.type
    if t in ("identifier","field_expression","integer_literal"): return True
    if t=="call_expression":
        return bool(re.search(r"\.(len|count|chars|bytes|position|find|rfind)\s*\(", txt(node,src)))
    if t in ("parenthesized_expression","type_cast_expression","cast_expression"):
        return bool(re.search(r"\b(usize|u8|u16|u32|u64|u128)\b", txt(node,src))) or any(
            _sizelike(c,src) for c in node.named_children)
    return False

def _dominates_ge(sub_node, a_text, b_text, src):
    """A dominating `a > b` / `a >= b` / `b < a` / `b <= a` (in if/while/&&) proves `a-b` safe."""
    pats=[rf'{re.escape(a_text)}\s*>=?\s*{re.escape(b_text)}',
          rf'{re.escape(b_text)}\s*<=?\s*{re.escape(a_text)}']
    def hit(s): return any(re.search(p,s) for p in pats)
    node=sub_node.parent
    while node is not None:
        if node.type in ("if_expression","while_expression","match_arm"):
            cond=node.child_by_field_name("condition") or node.child_by_field_name("value")
            if cond is not None and hit(txt(cond,src)): return True
        if node.type=="binary_expression":
            ops=[txt(c,src) for c in node.children if not c.is_named]
            if "&&" in ops and hit(txt(node,src)): return True
        if node.type=="function_item": break
        node=node.parent
    return False

def sub_underflow(sub_node, src):
    """(verdict,reason) for a bare `a - b` subtraction of two size-like, non-literal operands."""
    kids=[c for c in sub_node.named_children]
    if len(kids)!=2: return ("UNCERTAIN","not a-b")
    a,b=kids
    if a.type=="integer_literal" or b.type=="integer_literal":  # `x - 1` = index-offset shape, handled elsewhere
        return ("UNCERTAIN","literal operand")
    if not (_sizelike(a,src) and _sizelike(b,src)): return ("UNCERTAIN","not size-like")
    at,bt=txt(a,src),txt(b,src)
    if re.search(r"\bas\s+i(8|16|32|64|128|size)\b",txt(sub_node,src)):  # signed can't underflow-panic
        return ("SAFE","signed operands (goes negative, no panic)")
    if re.search(r"saturating_sub|checked_sub|wrapping_sub",txt(sub_node,src)): return ("SAFE","checked sub")
    if _dominates_ge(sub_node,at,bt,src): return ("UNCERTAIN",f"dominating `{at}>={bt}` guard")
    return ("UNGUARDED",f"usize underflow: `{at} - {bt}` with no dominating `{at}>={bt}`")

def sink_guard(kind, sink_node, letmap, src):
    """Return (verdict, reason)."""
    if kind=="arith:sub":
        return sub_underflow(sink_node, src)
    if kind.startswith("panic:index"):
        base,offs=_bounds_of_index(sink_node)
        base_text=txt(base,src) if base is not None else ""
        tags=[_offset_class(o,letmap,src,base_text) for o in offs]
        if not tags: return ("UNCERTAIN","no offsets")
        if any(t=="underflow" for t in tags):
            if dominating_len_guard(sink_node, base_text, src):
                return ("UNCERTAIN",f"underflow shape but dominating len-guard on `{base_text}`")
            if loop_bound_guard(sink_node, offs, src):
                return ("UNCERTAIN","underflow shape but loop-bound (for v in K.. ) clears `v-k`")
            if positivity_guard(sink_node, _underflow_vars(offs, src), src):
                return ("UNCERTAIN","underflow shape but index-positivity guard (x>0 && ...)")
            return ("UNGUARDED",f"underflow: {tags}")
        if all(t in ("find","clamp","bufread","satsub","literal") for t in tags):
            return ("SAFE",f"{tags}")
        return ("UNCERTAIN",f"{tags}")
    if kind in ("panic:step_by",):
        # arg is stored as the sink's offset via find_sinks; re-extract
        a=sink_node.child_by_field_name("arguments")
        E=a.named_children[0] if (a and a.named_children) else None
        if E is None: return ("UNCERTAIN","no arg")
        if outer_max_ge1(E,src) or E.type=="integer_literal": return ("SAFE","max/literal")
        if has_top_div(E,src): return ("UNGUARDED","zerodiv: x/n not outer-.max(>=1)")
        return ("UNCERTAIN","bare/var step")
    # unwrap/expect (Option/Result origin unknown), inject (path sanitation) -> verifier
    return ("UNCERTAIN","needs verifier")
