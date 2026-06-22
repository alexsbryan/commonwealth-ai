"""Lexer for minilang: turns source text into a flat token list."""

KEYWORDS = {"let", "in", "and", "not", "true", "false"}
SINGLE_CHAR_OPS = set("+-*/<>=()")


class Token:
    def __init__(self, kind, value):
        self.kind = kind  # 'NUM' | 'IDENT' | 'KW' | 'OP' | 'EOF'
        self.value = value

    def __repr__(self):
        return f"Token({self.kind}, {self.value!r})"


def tokenize(src):
    toks = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c.isspace():
            i += 1
            continue
        if c.isdigit():
            j = i
            while j < n and src[j].isdigit():
                j += 1
            toks.append(Token("NUM", int(src[i:j])))
            i = j
            continue
        if c.isalpha() or c == "_":
            j = i
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            word = src[i:j]
            toks.append(Token("KW" if word in KEYWORDS else "IDENT", word))
            i = j
            continue
        if c in SINGLE_CHAR_OPS:
            toks.append(Token("OP", c))
            i += 1
            continue
        raise SyntaxError(f"unexpected character {c!r} at position {i}")
    toks.append(Token("EOF", None))
    return toks
