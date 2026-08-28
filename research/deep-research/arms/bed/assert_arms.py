# SPDX-License-Identifier: AGPL-3.0-or-later
"""Assert the replay flew the arms that were asked for.

The COMPOSE_* variables cross a container boundary that silently drops them
(`toolbox run` does not forward the caller's environment). When they are lost
the harness falls back to its own defaults and produces a well-formed report
of a DIFFERENT experiment. Checking the request against the record is the
only thing that tells those two apart.
"""
import json
import sys

rep = json.load(open(sys.argv[1]))
want = [a.strip().replace(':', 'x') for a in sys.argv[2].split(',') if a.strip()]
got = [a['arm'] for a in rep['arms']]
if want != got:
    sys.exit('REFUSED: asked for arms %s but the harness flew %s — the '
             'environment did not cross into the container.' % (want, got))
print('    env crossed: flew %s' % got)
