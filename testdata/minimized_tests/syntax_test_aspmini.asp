' SYNTAX TEST "Packages/ASP/HTML-ASP.sublime-syntax"
<%
 Class TestClass2 Public Sub TestSub () Response.Write("wow") End Sub End Class
'^^^^^ meta.class.asp meta.class.identifier.asp storage.type.asp
'      ^ meta.class.asp meta.class.body.asp meta.class.asp meta.class.identifier.asp
'                ^ meta.class.asp meta.class.body.asp
%>
 <p>foobar</p>
'^^^ text.html.asp meta.tag.block.any.html - source.asp.embedded.html
