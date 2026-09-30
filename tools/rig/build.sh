#!/bin/sh
# Rebuild every rigged model (or the ones named) from art/models into
# aces-client/assets/models. One Blender at a time: they are memory-hungry.
#
#   tools/rig/build.sh                 # all
#   tools/rig/build.sh mig_15 su_25    # some
#   CHECK=/tmp/rig tools/rig/build.sh mig_15   # plus verification renders
set -e
cd "$(dirname "$0")/../.."
models=${*:-$(ls tools/rig/models/*.py | xargs -n1 basename | sed 's/\.py$//')}
for m in $models; do
    echo "== $m"
    if [ -n "$CHECK" ]; then
        blender -b --factory-startup -P tools/rig/rig.py -- "$m" --check "$CHECK" \
            | grep -E "^RIG|Error|Traceback" || true
    else
        blender -b --factory-startup -P tools/rig/rig.py -- "$m" \
            | grep -E "^RIG|Error|Traceback" || true
    fi
done
