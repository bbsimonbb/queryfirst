-- expect: belongs=nullable:expression
SELECT max(a.id) > 0 AS belongs FROM addresses a WHERE a.customer_id = $1
