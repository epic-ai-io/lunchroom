#!/bin/bash
# Inside the container: plays one card. The card is named by $CARD (1 to 6) or the first argument.
# $CARD_CELLS=base plays only the cell the spec's test line for the card names; $CARD_HARNESSES (a list of
# cc cx oc hm) narrows the grid of harnesses. Prints PASS, FAIL and MEASURED lines and one RESULT line.

set -u
. /opt/proof/lib.sh
. /opt/proof/lib-cards.sh

CARD=${1:-${CARD:-}}
case "$CARD" in
    1 | 2 | 3 | 4 | 5 | 6)
        echo "== card $CARD (cells: ${CARD_CELLS:-all}; harnesses: $(harness_list))"
        . "/opt/proof/card$CARD.sh"
        "card${CARD}_main"
        ;;
    *)
        echo "FAIL no card named: '$CARD' (1 to 6)"
        echo "RESULT: 0 PASS, 1 FAIL"
        exit 2
        ;;
esac
