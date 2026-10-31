-- expect: addresses.customerId=required:insert-notnull addresses.line1=required:insert-notnull addresses.city=nullable
/*
 @name insertAddresses
 @param addresses -> ((customerId, line1, city)...)
*/
INSERT INTO addresses (customer_id, line1, city) VALUES :addresses
