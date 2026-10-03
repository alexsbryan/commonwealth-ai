"""minilang: a tiny integer/boolean expression language.

Public API: `evaluate(source: str)` runs the full pipeline
(tokenize → parse → evaluate) and returns an int or bool.
"""

from .tokenizer import tokenize
from .parser import parse
from .evaluator import evaluate_ast


def evaluate(source):
    return evaluate_ast(parse(tokenize(source)), {})
