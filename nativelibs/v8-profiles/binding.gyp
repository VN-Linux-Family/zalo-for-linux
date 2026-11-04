{
  "targets": [
    {
      "target_name": "v8-profiles-native",
      "win_delay_load_hook": "false",
      "sources": [
        "src/cpu_profiler/cpu_profiler.cc",
        "src/cpu_profiler/cpu_profile.cc",
        "src/cpu_profiler/cpu_profile_node.cc",
        "src/heap_profiler/sampling_heap_profiler.cc",
        "src/heapsnapshot/heap_profiler.cc",
        "src/heapsnapshot/heap_snapshot.cc",
        "src/heapsnapshot/heap_output_stream.cc",
        "src/heapsnapshot/heap_graph_node.cc",
        "src/heapsnapshot/heap_graph_edge.cc",
        "src/profiler.cc",
        "src/environment_data.cc"
      ],
      "include_dirs": [
        "src",
        "<!(node -e \"require('nan')\")"
      ],
      "cflags_cc!": ["-fno-exceptions", "-fno-rtti"],
      "conditions": [
        ["OS == \"linux\"", {
          "cflags_cc": [
            "-O2",
            "-std=c++20",
            "-Wno-sign-compare",
            "-Wno-cast-function-type",
            "-Wno-unused-result"
          ]
        }]
      ]
    }
  ]
}
