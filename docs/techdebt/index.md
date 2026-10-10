# Technical debt

For someone changing the code who wants to know what is known to be owed.

An RFC records a decision. This records what is owed: something
measured or found that is worth fixing, with nothing decided yet about how. Each
entry says what was observed, where the evidence is, and what would retire it.
When the fix needs a design, it becomes an RFC and the entry links to it. When
it lands, the entry is marked **Paid** with the commit, not deleted. The next
person to see the same symptom should find that it was already seen.

| # | Debt | Found | Status |
| --- | --- | --- | --- |
| [TD-0001](/techdebt/0001-per-request-sql-and-profile-coverage) | Every request pays for SQL, unattributed waits and routing that are not its own work, and 321 of 372 API operations have never been profiled | 2026-10-09 | Open |
