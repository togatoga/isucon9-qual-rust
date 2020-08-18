use actix_session::{CookieSession, Session};
use actix_web::{http, web, App, HttpRequest, HttpResponse, HttpServer, ResponseError};
use async_recursion::async_recursion;
use serde::{Deserialize, Serialize};
use sqlx::mysql::{MySql, MySqlPoolOptions};
use sqlx::Row;

use sqlx::Executor;

use std::collections::HashMap;
use std::fmt;
use std::io::Write;

static DEFAULT_PAYMENT_SERVICE_URL: &str = "http://localhost:5555";
static DEFAULT_SHIPMENT_SERVICE_URL: &str = "http://localhost:7000";

const ITEM_MIN_PRICE: u32 = 100;
const ITEM_MAX_PRICE: u32 = 1000000;
static ITEM_PRICE_ERR_MSG: &str = "商品価格は100ｲｽｺｲﾝ以上、1,000,000ｲｽｺｲﾝ以下にしてください";

static ITEM_STATUS_ON_SALE: &str = "on_sale";
static ITEM_STATUS_TRADING: &str = "trading";
static ITEM_STATUS_SOLD_OUT: &str = "sold_out";
static ITEM_STATUS_STOP: &str = "stop";
static ITEM_STATUS_CANCEL: &str = "cancel";

const ITEMS_PER_PAGE: u32 = 48;
const TRANSACTIONS_PER_PAGE: u32 = 10;

#[derive(sqlx::FromRow)]
struct Config {
    name: String,
    val: String,
}
#[derive(Debug)]
struct CustomError {
    error: (http::StatusCode, String),
}
impl fmt::Display for CustomError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "status: {} msg: {}", self.error.0, self.error.1)
    }
}

impl ResponseError for CustomError {
    fn status_code(&self) -> http::StatusCode {
        self.error.0
    }
    fn error_response(&self) -> HttpResponse {
        #[derive(Serialize)]
        struct Error {
            error: String,
        };
        HttpResponse::build(self.status_code()).json(Error {
            error: self.error.1.to_string(),
        })
    }
}
#[derive(Deserialize)]
struct ReqInitialize {
    payment_service_url: String,
    shipment_service_url: String,
}

#[derive(Serialize)]
struct ResInitialize {
    campaign: i32,
    language: String,
}

#[derive(Serialize)]
struct ResNewItems {
    #[serde(skip_serializing_if = "Option::is_none")]
    root_category_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_category_name: Option<String>,
    has_next: bool,
    items: Vec<ItemSimple>,
}

#[derive(Serialize, Debug, sqlx::FromRow)]
struct TransactionEvidence {
    id: i64,
    seller_id: i64,
    buyer_id: i64,
    status: String,
    item_id: i64,
    item_name: String,
    item_price: u32,
    item_description: String,
    item_category_id: u32,
    item_root_category_id: u32,
    #[serde(skip_serializing)]
    created_at: sqlx::types::time::PrimitiveDateTime,
    #[serde(skip_serializing)]
    updated_at: sqlx::types::time::PrimitiveDateTime,
}

#[derive(Serialize, Debug, sqlx::FromRow)]
struct User {
    id: i64,
    account_name: String,
    #[serde(skip_serializing)]
    hashed_password: Vec<u8>,
    address: String,
    num_sell_items: u32,
    #[serde(skip_serializing)]
    last_bump: sqlx::types::time::PrimitiveDateTime,
    #[serde(skip_serializing)]
    created_at: sqlx::types::time::PrimitiveDateTime,
}

#[derive(Serialize, Debug, sqlx::FromRow)]
struct UserSimple {
    id: i64,
    account_name: String,
    num_sell_items: u32,
}

#[derive(Serialize, Debug, sqlx::FromRow)]
struct Item {
    id: i64,
    seller_id: i64,
    buyer_id: i64,
    status: String,
    name: String,
    price: u32,
    description: String,
    image_name: String,
    category_id: u32,
    #[serde(skip_serializing)]
    created_at: sqlx::types::time::PrimitiveDateTime,
    #[serde(skip_serializing)]
    updated_at: sqlx::types::time::PrimitiveDateTime,
}

#[derive(Serialize)]
struct ItemSimple {
    id: i64,
    seller_id: i64,
    seller: UserSimple,
    status: String,
    name: String,
    price: u32,
    image_url: String,
    category_id: u32,
    category: Category,
    created_at: i64,
}

#[derive(Serialize)]
struct ItemDetail {
    id: i64,
    seller_id: i64,
    seller: UserSimple,
    #[serde(skip_serializing_if = "Option::is_none")]
    buyer_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    buyer: Option<UserSimple>,
    status: String,
    name: String,
    price: u32,
    description: String,
    image_url: String,
    category_id: u32,
    category: Category,
    #[serde(skip_serializing_if = "Option::is_none")]
    transaction_evidence_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transaction_evidence_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shipping_status: Option<String>,
    created_at: i64,
}

#[derive(Serialize, Debug)]
struct Category {
    id: u32,
    parent_id: u32,
    category_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_category_name: Option<String>,
}

impl<'c> sqlx::FromRow<'c, sqlx::mysql::MySqlRow> for Category {
    fn from_row(row: &sqlx::mysql::MySqlRow) -> Result<Self, sqlx::Error> {
        let id: u32 = row.try_get("id")?;
        let parent_id: u32 = row.try_get("parent_id")?;
        let category_name: String = row.try_get("category_name")?;
        Ok(Category {
            id,
            parent_id,
            category_name,
            parent_category_name: None,
        })
    }
}

#[derive(Serialize)]
struct ResSetting {
    csrf_token: String,
    payment_service_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<User>,
    categories: Vec<Category>,
}

async fn get_user_simple_by_id(
    pool: &sqlx::MySqlPool,
    user_id: i64,
) -> Result<UserSimple, sqlx::Error> {
    let user: Result<User, sqlx::Error> = sqlx::query_as("SELECT * FROM `users` WHERE `id` = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await;
    match user {
        Ok(user) => Ok(UserSimple {
            id: user.id,
            account_name: user.account_name,
            num_sell_items: user.num_sell_items,
        }),
        Err(e) => Err(e),
    }
}

#[async_recursion]
async fn get_category_by_id(
    pool: &sqlx::MySqlPool,
    category_id: u32,
) -> Result<Category, sqlx::Error> {
    let category: Result<Category, sqlx::Error> =
        sqlx::query_as("SELECT * FROM `categories` WHERE `id` = ?")
            .bind(category_id)
            .fetch_one(pool)
            .await;
    match category {
        Ok(mut category) => {
            if category.parent_id != 0 {
                let parent_category = get_category_by_id(pool, category.parent_id)
                    .await
                    .map_or(None, |parent_category| Some(parent_category.category_name));
                category.parent_category_name = parent_category;
            }
            Ok(category)
        }
        Err(e) => {
            return Err(e);
        }
    }
}

#[derive(Deserialize)]
struct NewItemsQueryParams {
    item_id: Option<i64>,
    created_at: Option<i64>,
}

async fn get_new_items(
    params: web::Query<NewItemsQueryParams>,
    pool: web::Data<sqlx::MySqlPool>,
) -> Result<HttpResponse, CustomError> {
    let item_id = params.item_id.unwrap_or(0);
    let created_at = params.created_at.unwrap_or(0);
    let items: Result<Vec<Item>, _> = if item_id > 0 && created_at > 0 {
        // paging
        sqlx::query_as("SELECT * FROM `items` WHERE `status` IN (?,?) AND (`created_at` < ?  OR (`created_at` <= ? AND `id` < ?)) ORDER BY `created_at` DESC, `id` DESC LIMIT ?")
            .bind(ITEM_STATUS_ON_SALE)
            .bind(ITEM_STATUS_SOLD_OUT)
            .bind(chrono::NaiveDateTime::from_timestamp(created_at, 0))
            .bind(chrono::NaiveDateTime::from_timestamp(created_at, 0))
            .bind(item_id)
            .bind(ITEMS_PER_PAGE + 1).fetch_all(pool.get_ref()).await
    } else {
        //1st page
        sqlx::query_as("SELECT * FROM `items` WHERE `status` IN (?,?) ORDER BY `created_at` DESC, `id` DESC LIMIT ?")
            .bind(ITEM_STATUS_ON_SALE)
            .bind(ITEM_STATUS_SOLD_OUT)
            .bind(ITEMS_PER_PAGE + 1).fetch_all(pool.get_ref()).await
    };
    match items {
        Ok(items) => {
            let mut item_simples = vec![];
            for item in items.iter() {
                let seller = get_user_simple_by_id(pool.get_ref(), item.seller_id).await;
                if seller.is_err() {
                    eprintln!("{:?}", seller.unwrap_err());
                    return Err(CustomError {
                        error: (http::StatusCode::NOT_FOUND, "seller not found".to_string()),
                    });
                }
                let seller = seller.unwrap();

                let category = get_category_by_id(pool.get_ref(), item.category_id).await;
                if category.is_err() {
                    eprintln!("{:?}", category.unwrap_err());
                    return Err(CustomError {
                        error: (
                            http::StatusCode::NOT_FOUND,
                            "category not found".to_string(),
                        ),
                    });
                }
                let category = category.unwrap();

                item_simples.push(ItemSimple {
                    id: item.id,
                    seller_id: item.seller_id,
                    seller: seller,
                    status: item.status.clone(),
                    name: item.name.clone(),
                    price: item.price,
                    image_url: get_image_url(&item.image_name),
                    category_id: item.category_id,
                    category: category,
                    created_at: item.created_at.timestamp(),
                });
            }
            let has_next = if item_simples.len() > ITEMS_PER_PAGE as usize {
                item_simples.truncate(ITEMS_PER_PAGE as usize);
                true
            } else {
                false
            };

            let rni = ResNewItems {
                root_category_id: None,
                root_category_name: None,
                has_next: has_next,
                items: item_simples,
            };
            Ok(HttpResponse::Ok().json(rni))
        }
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
    }
}

async fn get_new_category_items(
    req: HttpRequest,
    info: web::Path<u32>,
    pool: web::Data<sqlx::MySqlPool>,
) -> Result<HttpResponse, CustomError> {
    let root_category_id = info.0;
    let root_category = get_category_by_id(pool.get_ref(), root_category_id).await;
    let root_category = match root_category {
        Ok(root_category) => root_category,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::BAD_REQUEST,
                    "category not found".to_string(),
                ),
            });
        }
    };
    let category_ids: Result<Vec<u32>, sqlx::Error> =
        sqlx::query("SELECT id FROM `categories` WHERE parent_id=?")
            .bind(root_category.id)
            .try_map(|row: sqlx::mysql::MySqlRow| {
                let id: u32 = row.try_get("id")?;
                Ok(id)
            })
            .fetch_all(pool.get_ref())
            .await;
    let category_ids = match category_ids {
        Ok(category_ids) => category_ids,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
    };
    let url = url::Url::parse(&req.uri().to_string()).unwrap();
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();

    let item_id = match query
        .get("item_id")
        .unwrap_or(&"".to_string())
        .parse::<i64>()
    {
        Ok(item_id) => item_id,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "item_id param error".to_string(),
                ),
            });
        }
    };

    let created_at = match query
        .get("created_at")
        .unwrap_or(&"".to_string())
        .parse::<i64>()
    {
        Ok(created_at) => created_at,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "created_at param error".to_string(),
                ),
            });
        }
    };

    let items = if item_id > 0 && created_at > 0 {
        // paging
        let items: Result<Vec<Item>, sqlx::Error> = sqlx::query_as(&format!(
            "SELECT * FROM `items` WHERE `status` IN (?,?) AND category_id IN ({}) AND (`created_at` < ?  OR (`created_at` <= ? AND `id` < ?)) ORDER BY `created_at` DESC, `id` DESC LIMIT ?"
            , &category_ids.iter().map(|x| x.to_string()).collect::<Vec<String>>().join(",")
        ))
        .bind(ITEM_STATUS_ON_SALE)
            .bind(ITEM_STATUS_SOLD_OUT)
            .bind(chrono::NaiveDateTime::from_timestamp(created_at, 0))
            .bind(chrono::NaiveDateTime::from_timestamp(created_at, 0))
            .bind(item_id)
            .bind(ITEMS_PER_PAGE + 1).fetch_all(pool.get_ref()).await;
        items
    } else {
        // 1st page
        let items = sqlx::query_as(&format!(
            "SELECT * FROM `items` WHERE `status` IN (?,?) AND category_id IN ({}) ORDER BY created_at DESC, id DESC LIMIT ?",
            &category_ids.iter().map(|x| x.to_string()).collect::<Vec<String>>().join(",")
        )).bind(ITEM_STATUS_ON_SALE)
            .bind(ITEM_STATUS_SOLD_OUT)
            .bind(ITEMS_PER_PAGE + 1).fetch_all(pool.get_ref()).await;
        items
    };
    let items = match items {
        Ok(items) => items,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
    };
    let mut item_samples = vec![];
    for item in items.iter() {
        let seller = get_user_simple_by_id(pool.get_ref(), item.seller_id).await;
        let seller = match seller {
            Ok(seller) => seller,
            Err(e) => {
                eprintln!("{:?}", e);
                return Err(CustomError {
                    error: (http::StatusCode::NOT_FOUND, "seller not found".to_string()),
                });
            }
        };

        let category = get_category_by_id(pool.get_ref(), item.category_id).await;
        let category = match category {
            Ok(category) => category,
            Err(e) => {
                eprintln!("{:?}", e);
                return Err(CustomError {
                    error: (
                        http::StatusCode::NOT_FOUND,
                        "category not found".to_string(),
                    ),
                });
            }
        };
        item_samples.push(ItemSimple {
            id: item.id,
            seller_id: item.seller_id,
            seller: seller,
            status: item.status.clone(),
            name: item.name.clone(),
            price: item.price,
            image_url: get_image_url(&item.image_name),
            category_id: item.category_id,
            category: category,
            created_at: item.created_at.timestamp(),
        });
    }

    let has_next = if items.len() > ITEMS_PER_PAGE as usize {
        item_samples.truncate(ITEMS_PER_PAGE as usize);
        true
    } else {
        false
    };

    let rni = ResNewItems {
        root_category_id: Some(root_category.id),
        root_category_name: Some(root_category.category_name),
        items: item_samples,
        has_next: has_next,
    };
    Ok(HttpResponse::Ok().json(rni))
}

async fn get_transactions(
    req: HttpRequest,
    session: Session,
    pool: web::Data<sqlx::MySqlPool>,
) -> Result<HttpResponse, CustomError> {
    let mut conn = pool.acquire().await.unwrap();
    let user = get_user(&session, &mut conn).await;
    let user = match user {
        Ok(user) => user,
        Err(e) => {
            return Err(e);
        }
    };
    let url = url::Url::parse(&req.uri().to_string()).unwrap();
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let item_id = match query
        .get("item_id")
        .unwrap_or(&"".to_string())
        .parse::<i64>()
    {
        Ok(item_id) => item_id,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "item_id param error".to_string(),
                ),
            });
        }
    };

    let created_at = match query
        .get("created_at")
        .unwrap_or(&"".to_string())
        .parse::<i64>()
    {
        Ok(created_at) => created_at,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "created_at param error".to_string(),
                ),
            });
        }
    };

    let mut tx = match pool.get_ref().begin().await {
        Ok(tx) => tx,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
    };
    let items: Result<Vec<Item>, _> = if item_id > 0 && created_at > 0 {
        // paging
        let items  = sqlx::query_as("SELECT * FROM `items` WHERE (`seller_id` = ? OR `buyer_id` = ?) AND `status` IN (?,?,?,?,?) AND (`created_at` < ?  OR (`created_at` <= ? AND `id` < ?)) ORDER BY `created_at` DESC, `id` DESC LIMIT ?")
        .bind(user.id)
        .bind(user.id)
        .bind(ITEM_STATUS_ON_SALE)
        .bind(ITEM_STATUS_TRADING)
        .bind(ITEM_STATUS_SOLD_OUT)

        .bind(ITEM_STATUS_CANCEL)
        .bind(ITEM_STATUS_STOP)
        .bind(chrono::NaiveDateTime::from_timestamp(created_at, 0))
        .bind(chrono::NaiveDateTime::from_timestamp(created_at, 0))
        .bind(item_id)
        .bind(TRANSACTIONS_PER_PAGE + 1)
        .fetch_all(&mut tx).await;
        items
    } else {
        // 1st page
        let items = sqlx::query_as("SELECT * FROM `items` WHERE (`seller_id` = ? OR `buyer_id` = ?) AND `status` IN (?,?,?,?,?) ORDER BY `created_at` DESC, `id` DESC LIMIT ?")
        .bind(user.id)
        .bind(user.id)
        .bind(ITEM_STATUS_ON_SALE)
        .bind(ITEM_STATUS_TRADING)
        .bind(ITEM_STATUS_SOLD_OUT)
        .bind(ITEM_STATUS_CANCEL)
        .bind(ITEM_STATUS_STOP)
        .bind(TRANSACTIONS_PER_PAGE + 1)
        .fetch_all(&mut tx).await;
        items
    };
    let items = match items {
        Ok(items) => items,
        Err(e) => {
            eprintln!("{:?}", e);
            tx.rollback().await.unwrap();
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
    };

    for item in items.iter() {
        let seller = get_user(&session, &mut tx).await;
    }
    tx.commit().await.unwrap();

    Ok(HttpResponse::Ok().finish())
}

async fn get_reports(pool: web::Data<sqlx::MySqlPool>) -> Result<HttpResponse, CustomError> {
    let transaction_evidences: Result<Vec<TransactionEvidence>, _> =
        sqlx::query_as("SELECT * FROM `transaction_evidences` WHERE `id` > 15007")
            .fetch_all(pool.get_ref())
            .await;

    match transaction_evidences {
        Ok(transaction_evidences) => Ok(HttpResponse::Ok().json(transaction_evidences)),
        Err(e) => {
            //Need to replace it with output_error_msg
            eprintln!("{:?}", e);
            Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            })
        }
    }
}

async fn post_initialize(
    ri: web::Json<ReqInitialize>,
    pool: web::Data<sqlx::MySqlPool>,
) -> Result<HttpResponse, CustomError> {
    let cmd = std::process::Command::new("../sql/init.sh").output();
    match cmd {
        Ok(output) => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(&output.stdout).unwrap();
            lock.write_all(&output.stderr).unwrap();
        }
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "exec init.sh error".to_string(),
                ),
            });
        }
    };

    let result = sqlx::query("INSERT INTO `configs` (`name`, `val`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `val` = VALUES(`val`)")
    .bind("payment_service_url")
    .bind(ri.payment_service_url.clone())
    .execute(pool.get_ref())
    .await;
    match result {
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
        _ => {}
    }

    let result = sqlx::query("INSERT INTO `configs` (`name`, `val`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `val` = VALUES(`val`)")
    .bind("shipment_service_url")
    .bind(ri.shipment_service_url.clone())
    .execute(pool.get_ref())
    .await;
    match result {
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
        _ => {}
    }
    return Ok(HttpResponse::Ok().json(ResInitialize {
        campaign: 0,
        language: "Rust".to_string(),
    }));
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    let host = std::env::var("MYSQL_HOST").unwrap_or("127.0.0.1".to_string());
    let port = std::env::var("MYSQL_PORT").unwrap_or("3306".to_string());
    match port.parse::<u32>() {
        Err(e) => {
            eprintln!(
                "failed to read DB port number from an environment variable MYSQL_PORT.\nError: {}",
                e.to_string()
            );
        }
        _ => {}
    }

    let user = std::env::var("MYSQL_USER").unwrap_or("isucari".to_string());
    let dbname = std::env::var("MYSQL_DBNAME").unwrap_or("isucari".to_string());
    let password = std::env::var("MYSQL_PASS").unwrap_or("isucari".to_string());

    let dsn = format!("mysql://{}:{}@{}:{}/{}", user, password, host, port, dbname);
    let pool = MySqlPoolOptions::new().connect(&dsn).await?;

    HttpServer::new(move || {
        App::new()
            .wrap(CookieSession::signed(&[0; 32]).secure(false))
            .data(pool.clone())
            .route("/reports.json", web::get().to(get_reports))
            .route("/initialize", web::post().to(post_initialize))
            .route("/new_items.json", web::get().to(get_new_items))
            .route(
                "/new_items/{root_category_id}.json",
                web::get().to(get_new_category_items),
            )
            //.route("/settings", web::get().to(get_settings))
            .service(actix_files::Files::new("/", "../public").index_file("index.html"))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await?;

    Ok(())
}

async fn get_settings(
    session: Session,
    pool: web::Data<sqlx::MySqlPool>,
) -> Result<HttpResponse, CustomError> {
    let csrf_token = get_csrf_token(&session);
    let mut conn = pool.acquire().await.unwrap();
    let user = match get_user(&session, &mut conn).await {
        Ok(user) => Some(user),
        _ => None,
    };

    let payment_service_url = get_payment_service_url(pool.get_ref()).await;

    let categories: Vec<Category> = match sqlx::query_as("SELECT * FROM categories")
        .fetch_all(pool.get_ref())
        .await
    {
        Ok(categories) => categories,
        Err(e) => {
            eprintln!("{:?}", e);
            return Err(CustomError {
                error: (
                    http::StatusCode::INTERNAL_SERVER_ERROR,
                    "db error".to_string(),
                ),
            });
        }
    };

    let ress = ResSetting {
        csrf_token: csrf_token,
        user: user,
        payment_service_url: payment_service_url,
        categories: categories,
    };
    Ok(HttpResponse::Ok().json(ress))
}

async fn get_config_by_name(
    name: &str,
    pool: &sqlx::MySqlPool,
) -> Result<Option<String>, sqlx::Error> {
    let config: Result<Config, _> = sqlx::query_as("SELECT * FROM `configs` WHERE `name` = ?")
        .bind(name)
        .fetch_one(pool)
        .await;
    match config {
        Ok(config) => {
            return Ok(Some(config.val));
        }
        Err(e) => match e {
            sqlx::Error::RowNotFound => {
                return Ok(None);
            }
            _ => {
                eprintln!("{:?}", e);
                return Err(e);
            }
        },
    }
}

async fn get_payment_service_url(pool: &sqlx::MySqlPool) -> String {
    let val = get_config_by_name(&"payment_service_url", pool)
        .await
        .unwrap_or(None)
        .unwrap_or(DEFAULT_PAYMENT_SERVICE_URL.to_string());
    val
}
fn get_csrf_token(session: &Session) -> String {
    return session
        .get::<String>("csrf_token")
        .unwrap_or(None)
        .unwrap_or("".to_string());
}

async fn get_user<'a, E>(session: &Session, pool: &'a mut E) -> Result<User, CustomError>
where
    &'a mut E: Executor<'a, Database = MySql>,
{
    let user_id = session.get::<i64>("user_id");
    if let Ok(Some(user_id)) = user_id {
        let user: Result<User, sqlx::Error> =
            sqlx::query_as("SELECT * FROM `users` WHERE `id` = ?")
                .bind(user_id)
                .fetch_one(pool)
                .await;
        match user {
            Ok(user) => {
                return Ok(user);
            }
            Err(e) => match e {
                sqlx::Error::RowNotFound => {
                    return Err(CustomError {
                        error: (http::StatusCode::NOT_FOUND, "user not found".to_string()),
                    });
                }
                _ => {
                    eprintln!("{:?}", e);
                    return Err(CustomError {
                        error: (
                            http::StatusCode::INTERNAL_SERVER_ERROR,
                            "db error".to_string(),
                        ),
                    });
                }
            },
        }
    } else {
        return Err(CustomError {
            error: (http::StatusCode::NOT_FOUND, "no session".to_string()),
        });
    }
}

fn get_image_url(image_name: &str) -> String {
    format!("/upload/{}", image_name)
}
