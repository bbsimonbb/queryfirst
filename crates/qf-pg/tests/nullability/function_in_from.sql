-- expect: email=notnull t=nullable:analyzer-gave-up
SELECT c.email, t FROM customers c, unnest(c.tags) AS t
