-- expect: key.customerId=nullable key.line1=nullable
/*
 @name byKey
 @param key -> (customerId, line1)
*/
SELECT id FROM addresses WHERE (customer_id, line1) = :key
