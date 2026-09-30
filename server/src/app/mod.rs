//! The server: startup, the routes, the middleware, the accept loop, the health check and the hourly maintenance.

pub mod health;
pub mod maintenance;
pub mod middleware;
pub mod routes;
pub mod serve;
pub mod startup;
