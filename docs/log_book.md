# Log Book Panel

The log book is mostly a read-only view of recent contacts. This
should receive data from the FT4/FT8 protocol decoder automatically,
but in future versions it'll need a manual entry ability for logging
other types of contacts.

A contact is logged automatically when the QSO engine sees `RR73`
received (we answered a station) or `RR73` sent (we called CQ). The
full lifecycle and the unresolved question of how to treat incomplete
QSOs are covered in [`qso_flow.md`](qso_flow.md).

The log book should also show logs from other stations on the network.
When other operators log their own contacts, those will be broadcast
as network messages. There will not be a shared database of contacts,
so log book sync won't be complete. It will be very helpful to know,
however, when a station has been worked by any operator on the local
network.

The log book panel will have some visual distinction between a contact
logged by this operator or another operator on the network. This may
be an icon next to the entry, or use of bold type vs. non-bold.

