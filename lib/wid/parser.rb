# frozen_string_literal: true

require_relative "nodes"

module Wid
  class Parser
    ParserError = Class.new(StandardError)

    UnexpectedTokenError = Class.new(ParserError) do
      attr_reader :token

      def initialize(token = nil, expected = nil)
        @token = token
        if token.nil?
          super("Unexpected end of input. Expected `#{expected.inspect}'")
        else
          super("Unexpected token #{token.inspect} at line #{token.line}, column #{token.column}. Expected `#{expected.inspect}'")
        end
      end

      def line = @token&.line

      def column = @token&.column
    end

    UnrecognizedTokenError = Class.new(ParserError) do
      attr_reader :token

      def initialize(token)
        @token = token
        super("Unrecognized token `#{token.type}(#{token.value})' at line #{token.line}, column #{token.column}")
      end
    end

    def initialize(tokens)
      @tokens = tokens
      @pos = 0
    end

    def self.parse(tokens) = new(tokens).parse

    def parse
      parse_program
    end

    private

    def advance(offset = 1)
      token = peek(offset)
      @pos += offset
      token
    end

    def backtrack(offset = 1)
      token = peek(-offset)
      @pos -= offset
      token
    end

    def consume(expected_type)
      if peek.nil?
        raise UnexpectedTokenError.new(nil, expected_type)
      end

      if peek&.type == expected_type
        advance
      else
        raise UnexpectedTokenError.new(peek, expected_type)
      end
    end

    def peek(offset = 1)
      pos = (@pos - 1) + offset
      return if pos < 0 || pos >= @tokens.size

      @tokens[pos]
    end

    def current_token = peek(0)

    def previous_token = peek(-1)

    # Main entry point
    #
    # Program
    #  : StatementList
    #  ;
    def parse_program
      Nodes::Program.new(parse_statement_list)
    end

    # Literal
    #  : NumericLiteral
    #  | StringLiteral
    #  | BooleanLiteral
    #  | NilLiteral
    #  ;
    def parse_literal
      case peek.type
      when :NUMBER then parse_numeric_literal
      when :STRING then parse_string_literal
      when :true then parse_boolean_literal(true)
      when :false then parse_boolean_literal(false)
      when :nil then parse_nil_literal
      else raise UnrecognizedTokenError.new(peek)
      end
    end

    def parse_boolean_literal(value)
      consume(value ? :true : :false)
      Nodes::BooleanLiteral.new(value)
    end

    def parse_nil_literal
      consume(:nil)
      Nodes::Nil.new
    end

    def parse_numeric_literal
      value = consume(:NUMBER).value
      Nodes::NumericLiteral.new(value)
    end

    def parse_string_literal
      value = consume(:STRING).value
      Nodes::StringLiteral.new(value)
    end

    # StatementList
    #  : Statement
    #  | StatementList Statement -> Statement Statement Statement Statement
    #  ;
    def parse_statement_list(terminators = [])
      statements = [parse_statement]

      terminators = Array(terminators)

      while peek && !terminators.include?(peek.type)
        statements << parse_statement
      end

      statements
    end

    # Statement
    #  : ExpressionStatement
    #  | BlockStatement
    #  | EmptyStatement
    #  | IfStatement
    #  | IterationStatement
    #  | FunctionDeclaration
    #  | ReturnStatement
    #  ;
    def parse_statement
      case peek.type
      when :";" then parse_empty_statement
      when :"{" then parse_block_statement(:"{", :"}")
      when :do then parse_block_statement(:do, :end)
      when :if then parse_if_statement
      when :while then parse_iteration_statement
      when :def then parse_function_declaration
      when :return then parse_return_statement
      else parse_expression_statement
      end
    end

    # FunctionDeclaration
    #  : 'def' Identifier '(' OptFormalParameterList ')' BlockStatement
    #  ;
    def parse_function_declaration
      consume(:def)
      name = parse_identifier
      consume(:"(")
      params = (peek&.type != :")") ? parse_formal_parameter_list : []
      consume(:")")
      body = parse_block_statement(nil, :end)

      Nodes::FunctionDeclaration.new(name, params, body)
    end

    # FormalParameterList
    #  : Identifier
    #  | FormalParameterList ',' Identifier
    #  ;
    def parse_formal_parameter_list
      params = [parse_identifier]

      while peek&.type == :","
        consume(:",")
        params << parse_identifier
      end

      params
    end

    # ReturnStatement
    #  : 'return' OptExpression
    #  ;
    def parse_return_statement
      consume(:return)
      argument = (peek&.type == :end) ? nil : parse_expression

      Nodes::ReturnStatement.new(argument)
    end

    # IterationStatement
    #  : WhileStatement
    #  | ForStatement TODO
    def parse_iteration_statement
      case peek.type
      when :while then parse_while_statement
      else raise UnrecognizedTokenError.new(peek)
      end
    end

    # WhileStatement
    # : 'while' Expression Statement
    # ;
    def parse_while_statement
      consume(:while)
      test = parse_expression
      body = parse_block_statement(nil, :end)

      Nodes::WhileStatement.new(test, body)
    end

    # IfStatement
    #  : 'if' Expression Statement
    #  | 'if' Expression Statement 'else' Statement
    #  ;
    def parse_if_statement
      consume(:if)
      condition = parse_expression

      consequent = if peek&.type == :then
        parse_block_statement(:then, [:else, :end])
      else
        parse_block_statement(nil, [:else, :end])
      end

      alternative = if current_token&.type == :else
        backtrack(1)
        parse_block_statement(:else, :end)
      end

      Nodes::IfStatement.new(condition, consequent, alternative)
    end

    # EmptyStatement
    #  : ';'
    #  ;
    def parse_empty_statement
      consume(:";")
      Nodes::EmptyStatement.new
    end

    # BlockStatement
    #  : '{' StatementList? '}'
    #  | do StatementList? end
    #  | else StatementList? end
    #  ;
    def parse_block_statement(opening, closing)
      consume(opening) if opening
      closing = Array(closing)

      statements = if closing.include?(peek.type)
        []
      else
        parse_statement_list(closing)
      end

      found = closing.find { _1 == peek.type }
      consume(found)
      Nodes::BlockStatement.new(statements)
    end

    # ExpressionStatement
    #  : Expression
    #  | Expression ';'
    #  ;
    def parse_expression_statement
      expression = parse_expression
      consume(:";") if peek&.type == :";"
      Nodes::ExpressionStatement.new(expression)
    end

    # Expression
    #  : Literal
    #  ;
    def parse_expression
      parse_assignment_expression
    end

    # RELATIONAL_OPERATOR: >, >=, <, <=
    #
    # RelationalExpression
    #  : AdditiveExpression
    #  | RelationalExpression RELATIONAL_OPERATOR RelationalExpression
    #  ;
    def parse_relational_expression
      parse_binary_expression(:additive_expression, :RELATIONAL_OPERATOR)
    end

    # AssignmentExpression
    #  : EqualityExpression
    #  | LeftHandSideExpression AssignmentOperator AssignmentExpression
    #  ;
    def parse_assignment_expression
      left = parse_logical_or_expression

      return left if !assignment_operator?(peek&.type)

      operator = parse_assignment_operator.value
      left = check_valid_assignment(left)
      right = parse_assignment_expression
      Nodes::AssignmentExpression.new(operator, left, right)
    end

    # LeftHandSideExpression
    #  : CallMemberExpression
    #  ;
    def parse_left_hand_side_expression
      parse_call_member_expression
    end

    # CallMemberExpression
    #  : MemberExpression
    #  | CallExpression
    #  ;
    def parse_call_member_expression
      member = parse_member_expression

      if peek&.type == :"("
        return parse_call_expression(member)
      end

      member
    end

    # CallExpression
    #  : Callee Arguments
    #  ;
    #
    # Callee
    #  : MemberExpression
    #  | CallExpression
    #  ;
    def parse_call_expression(callee)
      call_expression = Nodes::CallExpression.new(callee, parse_arguments)

      if peek&.type == :"("
        call_expression = parse_call_expression(call_expression)
      end

      call_expression
    end

    # Arguments
    #  | '(' OptArgumentList ')'
    #  ;
    def parse_arguments
      argument_list = []
      consume(:"(")
      argument_list = parse_argument_list unless peek&.type == :")"
      consume(:")")

      argument_list
    end

    # ArgumentList
    #  : AssignmentExpression
    #  | ArgumentList ',' AssignmentExpression
    #  ;
    def parse_argument_list
      argument_list = [parse_assignment_expression]

      while peek&.type == :","
        consume(:",")
        argument_list << parse_assignment_expression
      end

      argument_list
    end

    # MemberExpression
    #  : PrimaryExpression
    #  | MemberExpression '.' Identifier
    #  | MemberExpression '[' Expression ']'
    #  ;
    def parse_member_expression
      object = parse_primary_expression

      while peek&.type == :"." || peek&.type == :"["
        if peek&.type == :"["
          consume(:"[")
          property = parse_expression
          consume(:"]")
          object = Nodes::MemberExpression.new(true, object, property)
        else
          consume(:".")
          property = parse_identifier
          object = Nodes::MemberExpression.new(false, object, property)
        end

      end

      object
    end

    def check_valid_assignment(node)
      return node if node.is_a?(Nodes::Identifier) || node.is_a?(Nodes::MemberExpression)

      raise ParserError.new("Invalid left-hand assignment target")
    end

    # Identifier
    #  : IDENTIFIER
    #  ;
    def parse_identifier
      value = consume(:IDENTIFIER).value
      Nodes::Identifier.new(value)
    end

    # AssignmentOperator
    #  : SIMPLE_ASSIGN
    #  | COMPLEX_ASSIGN
    #  ;
    def parse_assignment_operator
      if peek.type == :SIMPLE_ASSIGN
        return consume(:SIMPLE_ASSIGN)
      end

      consume(:COMPLEX_ASSIGN)
    end

    # Logical OR expression.
    #
    #  x || y
    #
    #  LogicalORExpression
    #  : LogicalANDExpression LOGICAL_OR LogicalORExpression
    #  | LogicalORExpression
    #  ;
    def parse_logical_or_expression
      parse_logical_expression(:logical_and_expression, :LOGICAL_OR)
    end

    # Logical AND expression.
    #
    #  x && y
    #
    # LogicalANDExpression
    #  : EqualityExpression LOGICAL_AND LogicalANDExpression
    #  | EqualityExpression
    #  ;
    def parse_logical_and_expression
      parse_logical_expression(:equality_expression, :LOGICAL_AND)
    end

    def parse_logical_expression(type, operator_type)
      left = send(:"parse_#{type}")

      while peek&.type == operator_type
        operator = consume(operator_type).value
        right = send(:"parse_#{type}")
        left = Nodes::LogicalExpression.new(operator, left, right)
      end

      left
    end

    def parse_equality_expression
      parse_binary_expression(:relational_expression, :EQUALITY_OPERATOR)
    end

    def assignment_operator?(type)
      type == :SIMPLE_ASSIGN || type == :COMPLEX_ASSIGN
    end

    # AdditiveExpression
    #  : MultiplicativeExpression
    #  | AdditiveExpression ADDITIVE_OPERATOR Literal -> Literal ADDITIVE_OPERATOR Literal ADDITIVE_OPERATOR Literal
    #  ;
    def parse_additive_expression
      parse_binary_expression(:multiplicative_expression, :ADDITIVE_OPERATOR)
    end

    # MultiplicativeExpression
    #  : UnaryExpression
    #  | MultiplicativeExpression MULTIPLICATIVE_OPERATOR UnaryExpression -> PrimaryExpression MULTIPLICATIVE_OPERATOR PrimaryExpression MULTIPLICATIVE_OPERATOR PrimaryExpression
    #  ;
    def parse_multiplicative_expression
      parse_binary_expression(:unary_expression, :MULTIPLICATIVE_OPERATOR)
    end

    def parse_binary_expression(type, operator_type)
      left = send(:"parse_#{type}")

      while peek&.type == operator_type
        operator = consume(operator_type).value
        right = send(:"parse_#{type}")
        left = Nodes::BinaryExpression.new(operator, left, right)
      end

      left
    end

    # PrimaryExpression
    #  : Literal
    #  | ParenthesizedExpression
    #  | Identifier
    #  ;
    def parse_primary_expression
      return parse_literal if literal?(peek.type)

      case peek.type
      when :"(" then parse_parenthesized_expression
      when :IDENTIFIER then parse_identifier
      else parse_left_hand_side_expression
      end
    end

    # UnaryExpression
    #  : LeftHandSideExpression
    #  | ADDITIVE_OPERATOR UnaryExpression
    #  | LOGICAL_NOT UnaryExpression
    #  ;
    def parse_unary_expression
      operator = case peek.type
      when :ADDITIVE_OPERATOR then consume(:ADDITIVE_OPERATOR).value
      when :LOGICAL_NOT then consume(:LOGICAL_NOT).value
      end

      if operator
        argument = parse_unary_expression
        Nodes::UnaryExpression.new(operator, argument)
      else
        parse_left_hand_side_expression # Move to default
      end
    end

    def literal?(type)
      type == :NUMBER || type == :STRING || type == :true || type == :false || type == :nil
    end

    # ParenthesizedExpression
    #  : '(' Expression ')'
    #  ;
    def parse_parenthesized_expression
      consume(:"(")
      expression = parse_expression
      consume(:")")

      expression
    end
  end
end
