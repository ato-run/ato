#!/bin/sh
# The offline Node image has no Git. Answer only the single version query
# made by the verified aoiko build, after checking the detached checkout.
set -eu
[ "$#" -eq 3 ] && [ "$1" = rev-parse ] && [ "$2" = --short ] && [ "$3" = HEAD ] || exit 1
IFS= read -r commit < /src/.git/HEAD
[ "$commit" = 3c135266c87747ad86a29d72a038cbe5abff9205 ] || exit 1
printf '%s\n' 3c13526
