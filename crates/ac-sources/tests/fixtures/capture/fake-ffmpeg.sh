#!/bin/sh
# A stand-in for ffmpeg capturing from an avfoundation device, for the
# capture source's tests (src/capture/tests.rs). It ignores ffmpeg's
# options; the input the test names says what the device does:
#
#   refuse       a mode the camera lacks: avfoundation's refusal and a list
#                of 20 modes on stderr, no frame, exit 1
#   fail-later   three 64x36 RGBA frames, then 20 lines on stderr, exit 1
c='[avfoundation @ 0x7f8e1c604a80]'
case "$*" in
*refuse*)
    echo "$c Selected framerate (29.970030) is not supported by the device." >&2
    echo "$c Supported modes:" >&2
    i=1
    while [ "$i" -le 20 ]; do
        echo "$c   $((i * 96))x$((i * 54))@[5.000000 30.000000]fps" >&2
        i=$((i + 1))
    done
    echo "Error opening input: Input/output error" >&2
    exit 1
    ;;
*fail-later*)
    head -c $((64 * 36 * 4 * 3)) /dev/zero
    i=0
    while [ "$i" -lt 20 ]; do
        echo "late line $i" >&2
        i=$((i + 1))
    done
    exit 1
    ;;
esac
echo "fake-ffmpeg.sh: no device named in: $*" >&2
exit 2
