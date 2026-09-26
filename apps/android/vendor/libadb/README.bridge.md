# Bridge's libadb source

Imported from https://github.com/MuntashirAkon/libadb-android at tag 3.1.1,
commit c849886ebc6d48e7b46d967e78a6bb65c90c3b74. Copyright and license notices
remain in each source file and `LICENSES/`; Bridge uses the Apache-2.0 option
where upstream offers a choice, plus the listed BSD notices.

Bridge patches stream opening to wait on an acknowledged state under the same
monitor used by the connection reader. This prevents a loopback peer's response
from being lost before the opener begins waiting. Cancelled opens close their
stream, and concurrent opens allocate distinct IDs.

The Android instrumentation suite exercises real loopback shell, file transfer,
installation and cancellation. Keep transport changes here, at their owning code.
