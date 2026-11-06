# ROS 2 message definitions

Copies of ROS 2 Jazzy's `.msg` files, one per type, at the same `package/Type`
path as upstream. `src/record/mcap/msg.rs` joins a type with its dependencies into the
schema rosbag2 would write. Do not edit them: every byte, comments included,
goes into the mcap.

| File | Source |
|---|---|
| `std_msgs/Bool.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/std_msgs/msg/Bool.msg) |
| `std_msgs/String.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/std_msgs/msg/String.msg) |
| `std_msgs/Float64.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/std_msgs/msg/Float64.msg) |
| `std_msgs/Header.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/std_msgs/msg/Header.msg) |
| `sensor_msgs/JointState.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/sensor_msgs/msg/JointState.msg) |
| `sensor_msgs/CompressedImage.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/sensor_msgs/msg/CompressedImage.msg) |
| `geometry_msgs/TransformStamped.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/TransformStamped.msg) |
| `geometry_msgs/Transform.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/Transform.msg) |
| `geometry_msgs/Quaternion.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/Quaternion.msg) |
| `geometry_msgs/Vector3.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/Vector3.msg) |
| `tf2_msgs/TFMessage.msg` | [ros2/geometry2](https://github.com/ros2/geometry2/blob/f702874b1c8535d6a038230ab2cda0ba5d521ebd/tf2_msgs/msg/TFMessage.msg) |
| `builtin_interfaces/Time.msg` | [ros2/rcl_interfaces](https://github.com/ros2/rcl_interfaces/blob/7aa3caf43377ea6ad615bc1040832e2c7566bfbe/builtin_interfaces/msg/Time.msg) |
| `geometry_msgs/PoseStamped.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/PoseStamped.msg) |
| `geometry_msgs/Pose.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/Pose.msg) |
| `geometry_msgs/Point.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/Point.msg) |
| `geometry_msgs/TwistStamped.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/TwistStamped.msg) |
| `geometry_msgs/Twist.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/geometry_msgs/msg/Twist.msg) |
| `sensor_msgs/Joy.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/sensor_msgs/msg/Joy.msg) |
| `diagnostic_msgs/DiagnosticArray.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/diagnostic_msgs/msg/DiagnosticArray.msg) |
| `diagnostic_msgs/DiagnosticStatus.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/diagnostic_msgs/msg/DiagnosticStatus.msg) |
| `diagnostic_msgs/KeyValue.msg` | [ros2/common_interfaces](https://github.com/ros2/common_interfaces/blob/a941f14bb318d8d904505ed935ccbb97f24a70a4/diagnostic_msgs/msg/KeyValue.msg) |
| `rcl_interfaces/Log.msg` | [ros2/rcl_interfaces](https://github.com/ros2/rcl_interfaces/blob/7aa3caf43377ea6ad615bc1040832e2c7566bfbe/rcl_interfaces/msg/Log.msg) |
| `foxglove_msgs/CompressedVideo.msg` | [foxglove/foxglove-sdk](https://github.com/foxglove/foxglove-sdk/blob/29b72bd2771befa0dd4613c4671d211fc2785cb3/ros/src/foxglove_msgs/ros2/CompressedVideo.msg) |
| `yubi_go_msgs/UvcFrame.msg` | ours: no ROS type carries a UVC frame's PTS, SCR and bus position |
| `yubi_go_msgs/InputChanges.msg` | ours: `sensor_msgs/Joy` has one stamp for every input, not one per input |
