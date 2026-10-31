-- expect: belongs=notnull
SELECT count(*) > 0 AS belongs FROM addresses a WHERE a.customer_id = $1 AND a.id = $2
