-- expect: address.customerId=required:insert-notnull address.line1=required:insert-notnull address.city=nullable
/*
 @name insertAddress
 @param address -> (customerId, line1, city)
*/
INSERT INTO addresses (customer_id, line1, city) VALUES :address
