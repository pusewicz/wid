module Wid
  module AST
    class Node
      def to_h
        {type: self.class}
      end
    end
  end
end
