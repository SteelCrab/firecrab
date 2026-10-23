# Schedules

Schedule a MicroVM to start or stop once, or on a recurring calendar schedule.
The API executes schedules on its current host. Multi-host VM placement is separate.

## Dashboard

Open **Schedules** and select **New schedule**.
Choose an existing MicroVM, **Start VM** or **Stop VM**, and an execution time.
Use **Weekdays**, **Every day**, or **Choose days** for recurring work.
Use **One time** with the date and time pickers for a single execution.
The preview shows the selected VM, operation, timing, and time zone before saving.

Names are generated automatically. **More options** lets you set a custom name,
change a recurring schedule's IANA time zone, or save it disabled.
The default zone is the browser's local zone. One-time date and time inputs use that displayed zone
and are converted to an explicit UTC instant. Nonexistent daylight-saving times are rejected.
For imported or advanced expressions, select **Custom cron**. Existing custom expressions are preserved.

The list shows readable timing, the linked VM, and the next execution.
Use the switch to pause or resume future runs. Select a schedule to see execution results,
edit its settings, or delete it. Deleting a schedule also deletes its history and keeps the VM.
Consumed one-time schedules cannot be enabled again until their execution time is edited.

The VM detail panel also contains a **Schedules** section.
**Add schedule** preselects that VM. **Manage** opens its filtered schedule list.
Selecting an existing VM schedule opens that schedule's details directly.
VM links from a schedule open the matching VM detail panel.
These links preserve the selection through reload and browser history navigation.

## CLI

Replace `VM_UUID` with an existing MicroVM identifier from `firecrab vm list`.
Create separate schedules for morning startup and evening shutdown:

```sh
firecrab schedule create --name weekday-start --vm VM_UUID --action start \
  --cron '0 9 * * Mon-Fri' --time-zone Asia/Seoul
firecrab schedule create --name weekday-stop --vm VM_UUID --action stop \
  --cron '0 18 * * Mon-Fri' --time-zone Asia/Seoul
firecrab schedule create --name one-time-stop --vm VM_UUID --action stop \
  --at '2030-01-01T18:00:00+09:00'
firecrab schedule list --json
firecrab schedule get SCHEDULE_UUID
firecrab schedule runs SCHEDULE_UUID
firecrab schedule disable SCHEDULE_UUID
firecrab schedule enable SCHEDULE_UUID
firecrab schedule delete SCHEDULE_UUID
```

`update SCHEDULE_UUID` accepts the same configuration flags as `create` and replaces all fields.
Use `--disabled` when creating or updating a schedule to save it without enabling it.
All schedule commands honor the global `--api` and `--host` flags.

## Timing

The five fields are **minute, hour, day of month, month, weekday**.
Wildcards, lists, ranges, and steps are supported. Use named weekdays (`Sun` through `Sat`).
Numeric weekdays use **1 = Sunday through 7 = Saturday**.
If both day of month and weekday are restricted, **both must match**.
This differs from some system crontab implementations.

One-time timestamps require `Z` or a numeric UTC offset. They execute at that absolute instant.
Recurring schedules require an explicit IANA time zone. The host's local zone has no effect.
During daylight saving changes, nonexistent local times are skipped.
For a repeated local time, only the earlier UTC offset is used. The second copy is skipped.
Runs remain identified by their UTC instant.
The scheduler checks for due work once per second. It does not guarantee exact start times.

## Execution and recovery

- Schedules and run results are stored in the existing Firecrab SQLite database.
- Only one scheduler process may own a database. A second API using the same database fails startup.
- Up to four scheduled operations execute concurrently. The same schedule cannot overlap itself.
- Each due occurrence is durably claimed before its VM operation starts.
- VM start waits for the existing startup pipeline to finish. Stop uses the existing force-stop operation.
- Normal VM state rules apply. Starting an already running VM or stopping an inactive VM records a failure.
- An overdue one-time schedule executes once. Several missed recurring occurrences become one run, using the oldest saved due time. No backlog is replayed.
- Recurring schedules resume at the first future occurrence after completion. Occurrences that pass during an active run are skipped.
- A consumed one-time schedule becomes disabled. To reuse it, edit it to a new future time.
- Restarting the API marks previously active runs as `interrupted`. Their side effects may have completed. They are never automatically retried. Recurring schedules resume at a future occurrence.
- A failed operation is recorded without an immediate retry. A recurring schedule remains enabled for its next occurrence.
- A deleted target VM causes a failed run and disables the schedule.
- Disabling prevents future claims. It does not cancel an already claimed operation. Editing, enabling, and deletion are rejected while an operation is active; repeated enable/disable requests are idempotent.
- Enabling a paused schedule chooses a future occurrence and does not replay paused time. Expired one-time schedules must be edited first.
- Keep the API running for schedules to execute. On macOS and Windows, this requires the managed Debian environment to stay running.

## REST API

| Method | Path | Effect |
| --- | --- | --- |
| GET, POST | `/api/schedules` | List / create |
| GET, PUT, DELETE | `/api/schedules/{id}` | Read / replace / delete |
| PUT | `/api/schedules/{id}/enabled` | Set `{"enabled":true}` or `false` |
| GET | `/api/schedules/{id}/runs` | Latest 100 executions, newest first |

Create and replacement requests use the same body:

```json
{
  "name": "weekday-start",
  "vmId": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "action": "start",
  "enabled": true,
  "timing": {
    "kind": "cron",
    "expression": "0 9 * * Mon-Fri",
    "timeZone": "Asia/Seoul"
  }
}
```

For a one-time request, replace `timing` with `{"kind":"once","at":"2030-01-01T09:00:00+09:00"}`.
Omitting `enabled` defaults to `true`. Unknown fields and invalid actions are rejected.
Names must contain 1–64 UTF-8 bytes and no control characters.
An enabled schedule must have a future execution time.

Responses add `id`, `nextRunAtMs`, `createdAtMs`, `updatedAtMs`, and nullable `lastRun`.
`nextRunAtMs` is null while running, disabled, or consumed.
Run records contain `id`, `scheduleId`, `scheduledAtMs`, `startedAtMs`, nullable `finishedAtMs`, `status`, and nullable `error`.
All `*Ms` fields use Unix milliseconds. Status is `running`, `succeeded`, `failed`, or `interrupted`.
History retention is limited to 100 runs per schedule.

Validation failures return `400`. Missing schedules return `404`. Active-operation conflicts return `409`.
Existing [API request rules](../reference/api.md#request-rules) apply.
