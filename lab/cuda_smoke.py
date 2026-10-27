#!/usr/bin/env python3
"""Run actual integer vector addition on a passed-through Pascal GPU."""
import ctypes as C
import json
import time

cuda = C.CDLL('libcuda.so.1')
U = C.c_uint
I = C.c_int
P = C.c_void_p
DP = C.c_uint64


def bind(name, args):
    function = getattr(cuda, name)
    function.argtypes = args
    function.restype = I
    return function


error_name = bind('cuGetErrorName', [I, C.POINTER(C.c_char_p)])


def check(code):
    if code:
        name = C.c_char_p()
        error_name(code, C.byref(name))
        raise RuntimeError(f'CUDA {code}: {name.value.decode() if name.value else "unknown"}')


init = bind('cuInit', [U])
get_device = bind('cuDeviceGet', [C.POINTER(I), I])
device_name = bind('cuDeviceGetName', [P, I, I])
capability = bind('cuDeviceComputeCapability', [C.POINTER(I), C.POINTER(I), I])
create_context = bind('cuCtxCreate_v2', [C.POINTER(P), U, I])
destroy_context = bind('cuCtxDestroy_v2', [P])
allocate = bind('cuMemAlloc_v2', [C.POINTER(DP), C.c_size_t])
free = bind('cuMemFree_v2', [DP])
to_device = bind('cuMemcpyHtoD_v2', [DP, P, C.c_size_t])
to_host = bind('cuMemcpyDtoH_v2', [P, DP, C.c_size_t])
load = bind('cuModuleLoadData', [C.POINTER(P), P])
get_function = bind('cuModuleGetFunction', [C.POINTER(P), P, C.c_char_p])
launch = bind('cuLaunchKernel', [P, U, U, U, U, U, U, U, P, C.POINTER(P), P])
sync = bind('cuCtxSynchronize', [])

ptx = C.create_string_buffer(b'''
.version 6.0
.target sm_61
.address_size 64
.visible .entry vector_add(.param .u64 a, .param .u64 b, .param .u64 out, .param .u32 count) {
  .reg .pred %p<2>;
  .reg .b32 %r<8>;
  .reg .b64 %rd<8>;
  ld.param.u64 %rd1, [a];
  ld.param.u64 %rd2, [b];
  ld.param.u64 %rd3, [out];
  ld.param.u32 %r1, [count];
  mov.u32 %r2, %ctaid.x;
  mov.u32 %r3, %ntid.x;
  mov.u32 %r4, %tid.x;
  mad.lo.s32 %r5, %r2, %r3, %r4;
  setp.ge.u32 %p1, %r5, %r1;
  @%p1 bra DONE;
  mul.wide.u32 %rd4, %r5, 4;
  add.s64 %rd5, %rd1, %rd4;
  add.s64 %rd6, %rd2, %rd4;
  add.s64 %rd7, %rd3, %rd4;
  ld.global.u32 %r6, [%rd5];
  ld.global.u32 %r7, [%rd6];
  add.u32 %r6, %r6, %r7;
  st.global.u32 [%rd7], %r6;
DONE:
  ret;
}
''')

check(init(0))
device = I()
check(get_device(C.byref(device), 0))
name = C.create_string_buffer(128)
check(device_name(name, len(name), device))
major, minor = I(), I()
check(capability(C.byref(major), C.byref(minor), device))
assert (major.value, minor.value) == (6, 1), 'Expected the Pascal GTX 1070 Ti'
context = P()
check(create_context(C.byref(context), 0, device))
pointers = []
try:
    count = 262144
    a = (U * count)(*(index % 1024 for index in range(count)))
    b = (U * count)(*((index * 7) % 1024 for index in range(count)))
    output = (U * count)()
    for _ in range(3):
        pointer = DP()
        check(allocate(C.byref(pointer), C.sizeof(a)))
        pointers.append(pointer)
    check(to_device(pointers[0], C.cast(a, P), C.sizeof(a)))
    check(to_device(pointers[1], C.cast(b, P), C.sizeof(b)))
    module, function = P(), P()
    check(load(C.byref(module), C.cast(ptx, P)))
    check(get_function(C.byref(function), module, b'vector_add'))
    length = U(count)
    parameters = (P * 4)(*(C.addressof(value) for value in [*pointers, length]))
    start = time.monotonic()
    iterations = 0
    while time.monotonic() - start < 1.0:
        check(launch(function, (count + 255) // 256, 1, 1, 256, 1, 1, 0,
                     None, parameters, None))
        iterations += 1
        if iterations % 32 == 0:
            check(sync())
    check(sync())
    check(to_host(C.cast(output, P), pointers[2], C.sizeof(output)))
    assert all(output[index] == a[index] + b[index] for index in range(count)), 'GPU arithmetic mismatch'
    print(json.dumps({'gpu': name.value.decode(), 'compute_capability': f'{major.value}.{minor.value}',
                      'vector_elements': count, 'kernel_launches': iterations,
                      'correct_result': True, 'elapsed_seconds': round(time.monotonic()-start, 3)}))
finally:
    for pointer in pointers:
        free(pointer)
    destroy_context(context)
