#!/usr/bin/env bash
# First job on a fresh instance: packages, the run bundle, and a display.
# Leaves /root/run (lab binary, clips, scripts) and an X server on :0 drawn
# by the NVIDIA driver (falls back to Xvfb on :99 and says so).
set -uo pipefail
export DEBIAN_FRONTEND=noninteractive
A=/root/agent.py
cd /root
echo "== machine"
nvidia-smi --query-gpu=name,driver_version,memory.total,pci.bus_id --format=csv
lscpu | grep -E 'Model name|^CPU\(s\)'
free -g | head -2
dpkg -l | awk '/nvidia/ {print $2, $3}' | head -40

echo "== packages"
apt-get update -q >/dev/null
driver=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader | cut -d. -f1)
flavour=$(dpkg -l | grep -qE "nvidia-driver-$driver-server|nvidia-headless-$driver-server" && echo "-server" || echo "")
apt-get install -y -q --no-install-recommends \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav gstreamer1.0-tools gstreamer1.0-gl gstreamer1.0-vaapi \
  libxkbcommon-x11-0 libxkbcommon0 libxcb1 libvulkan1 vulkan-tools mesa-vulkan-drivers \
  xvfb xserver-xorg-core x11-utils x11-xserver-utils xdotool \
  "xserver-xorg-video-nvidia-$driver$flavour" "libnvidia-gl-$driver$flavour" \
  "libnvidia-decode-$driver$flavour" "libnvidia-encode-$driver$flavour" \
  nvidia-vaapi-driver vainfo ffmpeg python3 jq weston >/tmp/apt.log 2>&1 || { tail -20 /tmp/apt.log; }
tail -1 /tmp/apt.log

echo "== bundle"
mkdir -p /root/run && cd /root/run
python3 $A get files/run.tar /root/run.tar || exit 1
tar -xf /root/run.tar && rm /root/run.tar
ls

echo "== X on the NVIDIA GPU"
bus=$(nvidia-smi --query-gpu=pci.bus_id --format=csv,noheader | head -1)
# 00000000:00:05.0 -> PCI:0:5:0 (decimal)
IFS=':.' read -r _ b d f <<<"$bus"
busid="PCI:$((16#$b)):$((16#$d)):$((16#$f))"
cat >/etc/X11/xorg-nvidia.conf <<EOF
Section "ServerLayout"
    Identifier "layout"
    Screen 0 "screen"
EndSection
Section "Device"
    Identifier "gpu"
    Driver "nvidia"
    BusID "$busid"
    Option "AllowEmptyInitialConfiguration" "true"
    Option "HardDPMS" "false"
EndSection
Section "Screen"
    Identifier "screen"
    Device "gpu"
    DefaultDepth 24
    Option "AllowEmptyInitialConfiguration" "true"
    SubSection "Display"
        Depth 24
        Virtual 2560 1440
    EndSubSection
EndSection
EOF
nohup Xorg :0 -config /etc/X11/xorg-nvidia.conf -noreset +extension GLX +extension RANDR >/tmp/xorg.log 2>&1 &
sleep 4
if DISPLAY=:0 xdpyinfo >/dev/null 2>&1; then
  echo "Xorg :0 up on $busid"
  DISPLAY=:0 xrandr 2>&1 | head -5
  echo ":0" >/root/run/display
else
  echo "Xorg failed:"; grep -E '\(EE\)|\(WW\)' /tmp/xorg.log | head -20
  nohup Xvfb :99 -screen 0 2560x1440x24 >/tmp/xvfb.log 2>&1 &
  sleep 2; echo ":99" >/root/run/display
fi
export DISPLAY=$(cat /root/run/display)
echo "== vulkan on $DISPLAY"
vulkaninfo --summary 2>&1 | grep -E 'deviceName|driverName|apiVersion|deviceType' | head -12
echo "== gstreamer decoders"
gst-inspect-1.0 nvcodec 2>&1 | grep -E 'dec|enc' | head -30
gst-inspect-1.0 va 2>&1 | grep -E 'dec' | head -20
for e in nvh264dec nvh265dec nvav1dec nvvp9dec vah264dec avdec_h264; do
  printf '%s rank: ' $e; gst-inspect-1.0 $e 2>/dev/null | awk '/Rank/ {print $2,$3; exit}' || echo missing
done
LIBVA_DRIVER_NAME=nvidia vainfo 2>&1 | tail -12
echo "setup done"
