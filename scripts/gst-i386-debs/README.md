Build inputs for zcall-bridge/gst-i386/ (Ubuntu 26.04 i386 GStreamer +
libv4l stack, downloaded with `apt download` from the resolute archive).
NOT shipped in the package — scripts/setup-zcall-bridge.js extracts them
into zcall-bridge/gst-i386/ at build time; the extracted tree (including
usr/share/doc/*/copyright and LICENSE.md) IS shipped. Regenerate with:

  cd scripts/gst-i386-debs
  apt download libgstreamer1.0-0:i386 libgstreamer-plugins-base1.0-0:i386 \
    gstreamer1.0-plugins-base:i386 gstreamer1.0-plugins-good:i386 \
    libv4l-0t64:i386 libv4lconvert0t64:i386 liborc-0.4-0t64:i386
