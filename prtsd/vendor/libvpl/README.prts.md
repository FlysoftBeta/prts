Unmodified Intel libvpl API headers and dispatcher source from tag v2.17.0,
commit d77f9195cf495b937631607333288fd917ae8939.
Upstream: https://github.com/intel/libvpl/tree/v2.17.0
License: MIT, reproduced in LICENSE.

Only api/vpl and libvpl/src are vendored. build.rs follows the Windows source
list in upstream libvpl/CMakeLists.txt, statically linking the official
dispatcher. No generated bindings, SDK binaries or driver DLLs are vendored.
native/vpl.cpp provides the PRTS C ABI; keep changes out of these upstream files.
