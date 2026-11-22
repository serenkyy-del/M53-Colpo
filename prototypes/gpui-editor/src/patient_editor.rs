use gpui_kit::{prelude::*,*};
use gpui_kit::component::{ActiveTheme,Disableable,StyledExt,button::{Button,ButtonVariants},input::{Input,InputState},date_picker::{DatePicker,DatePickerState},menu::{DropdownMenu,PopupMenuItem},v_flex,h_flex};
use serde_json::{Value,json};
use uuid::Uuid;
#[derive(Clone)]
pub enum PatientEditEvent { Save{patient:Uuid,body:Value},Cancel }
pub struct PatientEditor {
    context:Value, family:Entity<InputState>,given:Entity<InputState>,patronymic:Entity<InputState>,birth:Entity<DatePickerState>,sex:Option<String>,busy:bool,note:String,
}
impl EventEmitter<PatientEditEvent> for PatientEditor {}
impl PatientEditor {
    pub fn new(context:Value,window:&mut Window,cx:&mut Context<Self>)->Self {
        let input=|key:&str,label:&str,window:&mut Window,cx:&mut Context<Self>|cx.new(|cx|InputState::new(window,cx).placeholder(label.to_owned()).default_value(context[key].as_str().unwrap_or("").to_owned()));
        let family=input("family_name","Фамилия",window,cx);let given=input("given_name","Имя",window,cx);let patronymic=input("patronymic","Отчество",window,cx);
        let birth=cx.new(|cx|m53_gpui_editor::calendar_locale::date_state(window,cx));
        let date=context["birth_date"].as_str().and_then(|s|s.parse::<chrono::NaiveDate>().ok());
        birth.update(cx,|s,cx|s.set_date(gpui_kit::component::calendar::Date::Single(date),window,cx));
        let sex=context["sex"].as_str().map(str::to_owned);
        Self{context,family,given,patronymic,birth,sex,busy:false,note:String::new()}
    }
    pub fn set_result(&mut self,note:String,cx:&mut Context<Self>){self.busy=false;self.note=note;cx.notify();}
}
impl Render for PatientEditor {
    fn render(&mut self,_:&mut Window,cx:&mut Context<Self>)->impl IntoElement {
        let label=match self.sex.as_deref(){Some("F")=>"Женский",Some("M")=>"Мужской",Some("O")=>"Другой",_=>"Не указан"};let entity=cx.entity();
        v_flex().gap_2().p_2().border_1().border_color(cx.theme().border).rounded_md()
            .child(div().font_semibold().child("Изменение ручной карточки"))
            .child(div().text_xs().child(format!("Карточка: {} {}",self.context["family_name"].as_str().unwrap_or(""),self.context["given_name"].as_str().unwrap_or(""))))
            .child(Input::new(&self.family).disabled(self.busy).aria_label("Фамилия пациента"))
            .child(Input::new(&self.given).disabled(self.busy).aria_label("Имя пациента"))
            .child(Input::new(&self.patronymic).disabled(self.busy).aria_label("Отчество пациента"))
            .child(div().text_sm().child("Дата рождения"))
            .child(DatePicker::new(&self.birth).disabled(self.busy).placeholder("Не указана"))
            .child(Button::new("edit-patient-sex").outline().label(label).disabled(self.busy).dropdown_menu(move|mut menu,_,_|{
                for (value,label) in [(None,"Не указан"),(Some("F"),"Женский"),(Some("M"),"Мужской"),(Some("O"),"Другой")] {
                    let entity=entity.clone();menu=menu.item(PopupMenuItem::new(label).on_click(move|_,_,cx|entity.update(cx,|s,cx|{if !s.busy{s.sex=value.map(str::to_owned);cx.notify();}})));
                }menu
            }))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Утверждённые протоколы сохранят прежние данные. После изменения потребуется повторная сверка пациента."))
            .when(!self.note.is_empty(),|d|d.child(div().text_sm().text_color(cx.theme().danger).child(self.note.clone())))
            .child(h_flex().gap_2()
                .child(Button::new("save-manual-patient").primary().label(if self.busy{"Сохраняем…"}else{"Сохранить"}).disabled(self.busy).on_click(cx.listener(|s,_,_,cx|{
                    if s.busy{return;}
                    let id=s.context["id"].as_str().and_then(|v|Uuid::parse_str(v).ok()).unwrap_or(Uuid::nil());
                    let version=match m53_client::patient_edit::edit_version(&s.context,id){Ok(v)=>v,Err(e)=>{s.note=e;cx.notify();return;}};
                    let family=s.family.read(cx).value().trim().to_owned();let given=s.given.read(cx).value().trim().to_owned();
                    if family.is_empty()||given.is_empty(){s.note="Укажите фамилию и имя".into();cx.notify();return;}
                    let patronymic=s.patronymic.read(cx).value().trim().to_owned();let birth=s.birth.read(cx).date_time().start().map(|d|d.format("%Y-%m-%d").to_string());
                    let body=json!({"expected_version":version,"family_name":family,"given_name":given,"patronymic":if patronymic.is_empty(){None}else{Some(patronymic)},"birth_date":birth,"sex":s.sex});
                    s.busy=true;s.note.clear();cx.emit(PatientEditEvent::Save{patient:id,body});cx.notify();
                })))
                .child(Button::new("cancel-manual-patient").outline().label("Отмена").disabled(self.busy).on_click(cx.listener(|_,_,_,cx|cx.emit(PatientEditEvent::Cancel)))))
    }
}
