-- expect: n=nullable:analyzer-gave-up
WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 3)
SELECT n FROM r
