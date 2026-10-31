-- expect: n=nullable
SELECT g FROM generate_series(1, 10) g WHERE g = :n
