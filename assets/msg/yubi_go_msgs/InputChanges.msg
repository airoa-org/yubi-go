# The controller inputs that changed since the previous read, each with the
# instant OpenXR says it changed (XrActionState.lastChangeTime), converted to
# the clock every other stamp in the file is on. header.stamp is when the app
# read them, which is up to one render frame later.
#
# Names are those of /quest/controller/joy_layout; the thumbstick's two axes
# share one change time.

std_msgs/Header header
string[] name
float32[] value
builtin_interfaces/Time[] changed
