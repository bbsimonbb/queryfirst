-- expect: access=nullable:analyzer-gave-up count=nullable:analyzer-gave-up
SELECT access, count(*) FROM customers GROUP BY ROLLUP (access)
