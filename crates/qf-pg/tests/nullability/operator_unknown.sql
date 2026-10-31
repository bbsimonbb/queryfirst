-- expect: k=nullable:analyzer-gave-up
SELECT '{"a":1}'::jsonb -> 'b' AS k
