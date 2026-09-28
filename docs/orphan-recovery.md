# Bounded orphan-drop recovery

When a gather stops after server-confirmed log removal, the native adapter remembers the log type and former block position for up to 15 seconds. That memory only limits where to search. A recovery candidate requires a fresh visible matching item within three blocks of the former block, a complete bounded entity scan, a reversible route, inventory space, and the usual survival and dimension guards.

At acceptance, the adapter repeats the visible-item and route checks. Recovery sends no mining command. It follows the route, confirms a matching inventory increase, and returns along the route before success. The attempt lasts at most five seconds and does not rearm itself after failure. Death or a world lifecycle change clears the opportunity. A missing item, changed route, unsafe step or absent inventory gain is reported as failure.

Native scenario and live verification remain pending.
