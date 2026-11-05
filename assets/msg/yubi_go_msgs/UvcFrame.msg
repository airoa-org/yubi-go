# One UVC frame's place on the camera's clock and on the USB bus, and the
# stream's health when it closed. Written beside each CompressedImage, with the
# same header and the same mcap sequence.
#
# header.stamp is the image's: when the URB holding the frame's first packet
# was reaped on the host. That is arrival, not exposure; pts and scr are what
# place the exposure.

std_msgs/Header header

# The camera's frame ordinal since the stream opened. Damaged frames are counted
# but never written, so a gap here is a frame that was lost.
uint64 frame

# dwPresentationTime: the camera's clock at the start of exposure.
# Meaningful only when has_pts.
bool has_pts
uint32 pts

# dwSourceClock: bits 0-31 the camera's clock, bits 32-42 the USB SOF frame
# number it was sampled at. Meaningful only when has_scr. The host side of
# that SOF is not available on Horizon OS, so this is kept as the camera sent it.
bool has_scr
uint64 scr

# When the URB holding the frame's first and last packets was reaped on the
# host, and each packet's index within its URB (one packet per 125 us
# microframe). Fitting pts against these places the exposure on the host clock.
builtin_interfaces/Time first_reaped
uint16 first_packet
builtin_interfaces/Time last_reaped
uint16 last_packet

# Payload bytes in this frame.
uint32 bytes

# Cumulative since the stream opened.
uint64 errors
uint64 bad_headers
uint64 damaged_frames
uint64 lost_packets
uint64 queue_drops
